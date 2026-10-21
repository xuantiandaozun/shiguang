"""Loopback-only SSH/SFTP fixture for the ignored Rust integration test.

Install paramiko into %APPDATA%/com.deskhelper.win/temp/ssh-test-python,
then run cargo test --lib servers::tests::loopback_ssh_sftp -- --ignored.
No system SSH service or remote account is used.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import socket
import sys
import threading
import time

sys.path.insert(0, str(Path(os.environ["APPDATA"]) / "com.deskhelper.win/temp/ssh-test-python"))
import paramiko

root = Path(sys.argv[1]).resolve()
root.mkdir(parents=True, exist_ok=True)
host_key = paramiko.RSAKey.generate(2048)
client_key = paramiko.RSAKey.generate(2048)
client_key.write_private_key_file(str(root / "client.pem"))


class SSH(paramiko.ServerInterface):
    def check_auth_password(self, username, password):
        return paramiko.AUTH_SUCCESSFUL if (username, password) == ("fixture", "fixture-password") else paramiko.AUTH_FAILED

    def check_auth_publickey(self, username, key):
        return paramiko.AUTH_SUCCESSFUL if username == "fixture" and key == client_key else paramiko.AUTH_FAILED

    def get_allowed_auths(self, username):
        return "password,publickey"

    def check_channel_request(self, kind, channel_id):
        return paramiko.OPEN_SUCCEEDED if kind == "session" else paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

    def check_channel_exec_request(self, channel, command):
        def execute():
            try:
                if command == b"fixture_slow":
                    time.sleep(3)
                elif command == b"fixture_large":
                    channel.sendall_stderr(b"e" * 131072)
                    channel.sendall(b"o" * 131072)
                else:
                    channel.sendall("fixture 输出\n".encode())
                channel.send_exit_status(7 if command == b"fixture_fail" else 0)
                channel.shutdown_write()
                channel.close()
            except (OSError, EOFError):
                pass
        threading.Thread(target=execute, daemon=True).start()
        return True


class SFTP(paramiko.SFTPServerInterface):
    def path(self, remote):
        p = (root / remote.lstrip("/")).resolve()
        if p != root and root not in p.parents:
            raise PermissionError(remote)
        return p

    def list_folder(self, path):
        try:
            result = []
            for p in self.path(path).iterdir():
                attr = paramiko.SFTPAttributes.from_stat(p.lstat())
                attr.filename = p.name
                result.append(attr)
            return result
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)

    def stat(self, path):
        try:
            return paramiko.SFTPAttributes.from_stat(self.path(path).stat())
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)

    lstat = stat

    def open(self, path, flags, attr):
        try:
            fd = os.open(self.path(path), flags | os.O_BINARY, attr.st_mode or 0o644)
            mode = "r+b" if flags & os.O_RDWR else "wb" if flags & os.O_WRONLY else "rb"
            f = os.fdopen(fd, mode)
            handle = paramiko.SFTPHandle(flags)
            handle.readfile = f
            handle.writefile = f
            return handle
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)

    def mkdir(self, path, attr):
        try:
            self.path(path).mkdir()
            return paramiko.SFTP_OK
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)

    def remove(self, path):
        try:
            self.path(path).unlink()
            return paramiko.SFTP_OK
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)

    def rename(self, old, new):
        try:
            self.path(old).rename(self.path(new))
            return paramiko.SFTP_OK
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)

    def posix_rename(self, old, new):
        try:
            self.path(old).replace(self.path(new))
            return paramiko.SFTP_OK
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)


def serve(client):
    transport = paramiko.Transport(client)
    # Exercise the Windows libssh2 CNG backend with a fixed supported KEX.
    transport.get_security_options().kex = ("diffie-hellman-group14-sha256",)
    transport.add_server_key(host_key)
    transport.set_subsystem_handler("sftp", paramiko.SFTPServer, SFTP)
    try:
        transport.start_server(server=SSH())
        while transport.is_active():
            time.sleep(0.05)
    except (EOFError, OSError, paramiko.SSHException):
        pass
    finally:
        transport.close()


listener = socket.socket()
listener.bind(("127.0.0.1", 0))
listener.listen(16)
print(json.dumps({"port": listener.getsockname()[1], "fingerprint": "SHA256:" + base64.b64encode(hashlib.sha256(host_key.asbytes()).digest()).decode().rstrip("=")}), flush=True)
while True:
    client, _ = listener.accept()
    threading.Thread(target=serve, args=(client,), daemon=True).start()
