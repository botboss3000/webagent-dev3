"""Offline package smoke checks for the published Git Control adapter."""
import asyncio
import importlib.util
import hashlib
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("git_control_package_adapter", ROOT / "adapter.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
for name in ("adapter.py", "transport.py", "file_manager.py", "askpass.py"):
    compile((ROOT / name).read_bytes(), str(ROOT / name), "exec")
assert not module.secret(b"https://user:password@deb.debian.org/debian/pool/p.deb")
assert module.secret(b"https://user:password@service.example/private")
assert module.secret(b"access_token=" + b"x" * 24)
assert module.secret(b"-----BEGIN PRIVATE KEY-----")
async def push_guard_check():
    with tempfile.TemporaryDirectory() as temporary:
        base=Path(temporary)
        repo=base/"webagent-v3"
        repo.mkdir()
        def git(*args):
            return subprocess.run(["git",*args],cwd=repo,capture_output=True,check=True).stdout.decode().strip()
        git("init","-q","--initial-branch=main")
        git("config","user.name","Fixture")
        git("config","user.email","fixture@example.invalid")
        (repo/"README.txt").write_text("baseline\n")
        git("add","README.txt")
        git("commit","-qm","baseline")
        remote=base/"remote.git"
        subprocess.run(["git","init","--bare","-q",str(remote)],check=True)
        git("remote","add","origin",str(remote))
        (repo/"fixture.txt").write_bytes(b"https://u:"+b"very-secret@service.example/path\n")
        git("add","fixture.txt")
        git("commit","-qm","guard fixture")
        result=await module.execute("git_tool",{"operation":"push","expected_head":git("rev-parse","HEAD")},base)
        assert result["error_kind"]=="git_credential_guard",result
        assert result["diagnostics"]["status"]=="located",result
        assert result["diagnostics"]["path"]=="fixture.txt",result
        assert result["diagnostics"]["commit"]==git("rev-parse","HEAD"),result
        assert "very-secret" not in str(result)
        assert subprocess.run(["git","--git-dir",str(remote),"rev-parse","--verify","main"],capture_output=True).returncode!=0

async def reviewed_history_check():
    with tempfile.TemporaryDirectory() as temporary:
        base=Path(temporary)
        repo=base/"webagent-v3";repo.mkdir()
        def git(*args):
            return subprocess.run(["git",*args],cwd=repo,capture_output=True,check=True).stdout.decode().strip()
        git("init","-q","--initial-branch=main")
        git("config","user.name","Fixture")
        git("config","user.email","fixture@example.invalid")
        (repo/"README.txt").write_text("baseline\n")
        git("add","README.txt");git("commit","-qm","baseline")
        baseline=git("rev-parse","HEAD")
        git("update-ref","refs/remotes/origin/main",baseline)
        fixture=b'assert secret(b"https://u:' + b'fixture-value@service.example/path")\n'
        (repo/"fixture.txt").write_bytes(fixture)
        git("add","fixture.txt");git("commit","-qm","fixture")
        commit=git("rev-parse","HEAD")
        digest=hashlib.sha256(b"+"+fixture).hexdigest()
        args=["show","--format=","--no-ext-diff","--no-textconv","--no-renames","--unified=0","-p",commit,"--","fixture.txt"]
        try:await module.command(repo,args,scan_only=True)
        except module.Rejected as error:assert error.kind=="git_credential_guard"
        else:raise AssertionError("unreviewed fixture passed")
        await module.command(repo,args,scan_only=True,reviewed_lines={digest})
        module._REVIEWED_HISTORY_LINES[(commit,"fixture.txt")]=frozenset({digest})
        await module.scan_outgoing_history(repo,"origin",commit)
        module._REVIEWED_HISTORY_LINES.pop((commit,"fixture.txt"))
        try:await module.scan_outgoing_history(repo,"origin",commit)
        except module.Rejected as error:
            assert error.kind=="git_credential_guard"
            assert error.diagnostics["commit"]==commit
            assert error.diagnostics["path"]=="fixture.txt"
            assert error.diagnostics["line"]==1
        else:raise AssertionError("exception applied outside exact commit/path")
        module._REVIEWED_HISTORY_LINES[(commit,"fixture.txt")]=frozenset({digest})
        extra=b"another=https://u:"+b"unreviewed-value@service.example/path\n"
        (repo/"fixture.txt").write_bytes(fixture+extra)
        git("add","fixture.txt");git("commit","-qm","new finding")
        try:await module.scan_outgoing_history(repo,"origin",git("rev-parse","HEAD"))
        except module.Rejected as error:
            assert error.kind=="git_credential_guard"
            assert error.diagnostics["path"]=="fixture.txt"
        else:raise AssertionError("new credential-shaped content passed")
        (repo/"fixture.txt").write_bytes(fixture)
        git("add","fixture.txt");git("commit","-qm","remove finding")
        removal=git("rev-parse","HEAD")
        await module.command(repo,["show","-m","--format=","--no-ext-diff","--no-textconv","--no-renames","--unified=0","-p",removal,"--","fixture.txt"],scan_only=True)
        module._REVIEWED_HISTORY_LINES.pop((commit,"fixture.txt"))

asyncio.run(push_guard_check())
asyncio.run(reviewed_history_check())
print("Git Control package checks passed")
