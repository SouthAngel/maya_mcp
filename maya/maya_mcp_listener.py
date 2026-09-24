# -*- coding: utf-8 -*-
"""maya_mcp_listener.py -- 在已打开的 Maya 内部运行的 TCP listener。

供 maya-mcp (Rust MCP 服务) 连接,在 Maya 主线程中执行 Python / MEL 代码并返回结果。
兼容 Python 2.6/2.7 (Maya 2014~2020) 与 Python 3 (Maya 2022+)。

启动方式(Maya Script Editor, Python 标签页执行一次):
    p = r"<本文件路径>"
    exec(compile(open(p, "rb").read(), p, "exec"))

说明: 必须以 "rb" 读取字节流再 compile,Py2/Py3 均可靠 --
    Py2 对含编码声明的 Unicode 字符串 exec 会报
    "SyntaxError: encoding declaration in Unicode string";
    Py3 直接 open() 文本模式则受系统区域编码影响(中文 Windows 为 cp936)。

也可加入 userSetup.py 实现随 Maya 自启:
    import maya.cmds as cmds
    cmds.evalDeferred(
        "p = r'<本文件路径>'; exec(compile(open(p, 'rb').read(), p, 'exec'))",
        lowestPriority=True,
    )

协议: 每行一个 JSON 对象(UTF-8, 以 \\n 结尾)。
    请求: {"id": 1, "op": "ping"|"python"|"mel", "code": "...", "args": {...}(可选,仅 python)}
    响应: {"id": 1, "ok": true, "result": ..., "stdout": "..."}
          {"id": 1, "ok": false, "error": "...", "traceback": "...", "stdout": "..."}

python 执行命名空间已预置: cmds、mel、om2,以及请求参数 __mcp_args__ (dict)。
多语句支持: 最后一条"表达式语句"的值会作为 result 返回。
注意: print/log/异常/协议错误消息一律使用英文(规避 Py2 终端编码问题)。
"""

import ast
import json
import os
import socket
import sys
import threading
import traceback

try:
    import builtins  # Py3
except ImportError:
    import __builtin__ as builtins  # Py2

if sys.version_info[0] >= 3:
    from io import StringIO  # Py3: 仅接受 unicode
else:
    from StringIO import StringIO  # Py2: 同时接受 str/unicode

try:
    from contextlib import redirect_stdout  # Py3.4+
except ImportError:
    class redirect_stdout(object):
        """Py2/Py3.3 以下的替代实现。"""

        def __init__(self, new_target):
            self._new_target = new_target
            self._old_target = None

        def __enter__(self):
            self._old_target = sys.stdout
            sys.stdout = self._new_target
            return self._new_target

        def __exit__(self, exc_type, exc_val, exc_tb):
            sys.stdout = self._old_target
            return False

try:
    _STRING_TYPES = basestring  # Py2: str + unicode
except NameError:
    _STRING_TYPES = str  # Py3

DEFAULT_HOST = "127.0.0.1"
DEFAULT_PORT = 5055

_NO_RESULT = object()
# RLock: start(force=True) 在持锁状态下会调用 stop(), 非重入 Lock 会自死锁
_state = {"server": None, "thread": None, "lock": threading.RLock()}


def _log(msg):
    print("[maya-mcp] %s" % msg)


def _jsonable(obj, _depth=0):
    """把任意对象转换为可 JSON 序列化的结构;无法转换时退化为 repr。"""
    if _depth > 8:
        return repr(obj)
    if obj is None or isinstance(obj, (bool, int, float, _STRING_TYPES)):
        return obj
    try:
        json.dumps(obj)
        return obj
    except Exception:
        pass
    if isinstance(obj, dict):
        return dict((str(k), _jsonable(v, _depth + 1)) for k, v in list(obj.items())[:500])
    if isinstance(obj, (list, tuple, set)):
        return [_jsonable(v, _depth + 1) for v in list(obj)[:2000]]
    if isinstance(obj, bytes):
        return obj.decode("utf-8", "replace")
    r = repr(obj)
    if len(r) > 5000:
        r = r[:5000] + "...[truncated]"
    return r


def _run_python(code, args):
    import maya.cmds as cmds
    import maya.mel as mel

    try:
        from maya.api import OpenMaya as om2
    except Exception:
        om2 = None

    try:
        tree = ast.parse(code, "<maya-mcp>", "exec")
    except SyntaxError as e:
        return {"ok": False, "error": "SyntaxError: %s" % e}

    glb = {
        "__name__": "__maya_mcp__",
        "__doc__": None,
        "__builtins__": builtins,
        "cmds": cmds,
        "mel": mel,
        "om2": om2,
        "__mcp_args__": args if isinstance(args, dict) else {},
    }

    stdout_io = StringIO()
    try:
        result = _NO_RESULT
        with redirect_stdout(stdout_io):
            if tree.body and isinstance(tree.body[-1], ast.Expr):
                last = tree.body[-1]
                tree.body = tree.body[:-1]
                if tree.body:
                    exec(compile(tree, "<maya-mcp>", "exec"), glb)
                result = eval(compile(ast.Expression(last.value), "<maya-mcp>", "eval"), glb)
            else:
                exec(compile(tree, "<maya-mcp>", "exec"), glb)
        payload = {"ok": True, "stdout": stdout_io.getvalue()}
        if result is not _NO_RESULT:
            payload["result"] = _jsonable(result)
        return payload
    except Exception:
        return {
            "ok": False,
            "error": "".join(traceback.format_exception_only(*sys.exc_info()[:2])).strip(),
            "traceback": traceback.format_exc(),
            "stdout": stdout_io.getvalue(),
        }


def _run_mel(code):
    import maya.mel as mel

    try:
        result = mel.eval(code)
        return {"ok": True, "result": _jsonable(result)}
    except Exception as e:
        return {"ok": False, "error": str(e), "traceback": traceback.format_exc()}


def _run_in_main_thread(op, code, args):
    """监听线程调用:把执行调度到 Maya 主线程并阻塞等待结果。"""

    def _runner():
        # 永不抛异常,统一返回 payload dict
        try:
            if op == "python":
                return _run_python(code, args)
            if op == "mel":
                return _run_mel(code)
            if op == "ping":
                import maya.cmds as cmds

                return {"ok": True, "result": "pong (Maya %s)" % cmds.about(version=True)}
            return {"ok": False, "error": "unknown op: %r" % (op,)}
        except Exception:
            return {"ok": False, "error": "internal error", "traceback": traceback.format_exc()}

    try:
        import maya.utils

        return maya.utils.executeInMainThreadWithResult(_runner)
    except Exception:
        return {
            "ok": False,
            "error": "cannot dispatch to Maya main thread",
            "traceback": traceback.format_exc(),
        }


def _handle_line(line):
    """处理一行请求, 返回 (payload, op)。op 用于心跳静默判断。"""
    try:
        req = json.loads(line.decode("utf-8"))
    except Exception as e:
        return {"id": None, "ok": False, "error": "bad request: %s" % e}, None
    req_id = req.get("id")
    op = req.get("op")
    code = req.get("code", "")
    if op in ("python", "mel") and not isinstance(code, _STRING_TYPES):
        return {"id": req_id, "ok": False, "error": "code must be a string"}, op
    payload = _run_in_main_thread(op, code, req.get("args"))
    payload["id"] = req_id
    return payload, op


def _handle_connection(conn):
    peer = conn.getpeername()
    quiet = False  # 心跳(ping)连接全程静默
    try:
        rfile = conn.makefile("rb")
        first = True
        while True:
            line = rfile.readline()
            if not line:
                break
            resp, op = _handle_line(line)
            if first:
                first = False
                quiet = op == "ping"
                if not quiet:
                    _log("client connected: %s:%s" % (peer[0], peer[1]))
            conn.sendall(json.dumps(resp, ensure_ascii=False).encode("utf-8") + b"\n")
    except Exception:
        pass
    finally:
        try:
            conn.close()
        except Exception:
            pass
        if not quiet:
            _log("client disconnected: %s:%s" % (peer[0], peer[1]))


def _accept_loop(sock):
    while True:
        try:
            conn, _addr = sock.accept()
        except socket.error:
            break  # socket 已被 stop() 关闭
        thread = threading.Thread(target=_handle_connection, args=(conn,), name="maya-mcp-conn")
        thread.daemon = True
        thread.start()


def _maya_version():
    """Maya 版本号(无 maya 环境时返回 unknown)。"""
    try:
        import maya.cmds as cmds
        return str(cmds.about(version=True))
    except Exception:
        return "unknown"


def start(port=None, host=None, force=False):
    """启动 listener(幂等)。已运行时除非 force=True 否则跳过。"""
    with _state["lock"]:
        if _state["server"] is not None:
            if force:
                _log("init: force=True, restarting ...")
                stop()
            else:
                _log("already running on %s:%s" % _state["server"].getsockname()[:2])
                return
        if port is None:
            try:
                port = int(os.environ.get("MAYA_MCP_PORT", ""))
            except ValueError:
                port = 0
            if port <= 0:
                port = DEFAULT_PORT
        if host is None:
            host = os.environ.get("MAYA_MCP_HOST") or DEFAULT_HOST
        _log("init: maya=%s python=%s" % (_maya_version(), sys.version.split()[0]))
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        if hasattr(socket, "SO_EXCLUSIVEADDRUSE"):
            # Windows: 禁止第二个 socket(即使带 SO_REUSEADDR)抢绑同端口, 避免双 listener 并存
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
        else:
            # POSIX: 允许重启时立即越过 TIME_WAIT 重新绑定
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            sock.bind((host, port))
        except socket.error as e:
            sock.close()
            _log("init: bind failed on %s:%s (%s)" % (host, port, e))
            raise RuntimeError(
                "cannot bind %s:%s (%s). Port may be in use, try: start(port=xxxx)" % (host, port, e)
            )
        sock.listen(8)
        _state["server"] = sock
        thread = threading.Thread(
            target=_accept_loop, args=(sock,), name="maya-mcp-listener"
        )
        thread.daemon = True
        thread.start()
        _log("init: success - listening on %s:%s (ops: ping / python / mel)" % (host, port))


def stop():
    with _state["lock"]:
        sock = _state["server"]
        _state["server"] = None
    if sock is not None:
        try:
            sock.close()
        except Exception:
            pass
        _log("stopped")


def status():
    sock = _state["server"]
    if sock is None:
        print("[maya-mcp] stopped")
    else:
        print("[maya-mcp] running on %s:%s" % sock.getsockname()[:2])


# 直接 exec 本文件即自动启动(幂等)
# GUI Maya 延迟到主线程空闲时执行(userSetup 阶段 Maya 尚在初始化);
# batch/standalone 模式 deferred 回调不会触发, 直接启动; 无 Maya 环境同样直接启动
def _auto_start():
    try:
        import maya.cmds
        import maya.utils
        if maya.cmds.about(batch=True):
            start()
        else:
            maya.utils.executeDeferred(start)
    except Exception:
        start()


_auto_start()
