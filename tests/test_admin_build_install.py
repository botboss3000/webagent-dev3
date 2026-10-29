import hashlib
import sqlite3
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import socket
import struct
import threading
import types
import unittest
from unittest.mock import Mock, patch
import uuid

PATH = Path(__file__).resolve().parents[1] / "build-service/install.py"
spec = importlib.util.spec_from_file_location("admin_build_install", PATH)
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class FakePrivileges:
    def __init__(self):
        self.owners = {}
        self.euid = 0
    def uid(self): return self.euid
    def owner(self, info): return self.owners.get(info.st_ino, 1000)
    def chown(self, fd, uid, gid): self.owners[os.fstat(fd).st_ino] = uid


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.v3 = self.root / "v3"
        self.source = self.v3 / "build-service"
        self.source.mkdir(parents=True)
        self.host = b"CONTEXT = ('v3/src',)\n"
        (self.source / "host.py").write_bytes(self.host)
        (self.source / "install.py").write_bytes(b"# fixture bootstrap\n")
        (self.source / "release.py").write_bytes(b"# fixture release\n")
        (self.source / "performance.py").write_bytes(b"# fixture protected performance\n")
        for name in ("config.py","workspace.py","workspace_support.py","workspace_executor.py","lifecycle.py","reconfigure.py","deploy.py","buildctl.py","ownership.py","scheduler.py","resources.py","resource_control.py","docker_retention.py","docker_guard_dispatch.py","executor.py","engine_components.py","source_status.py","monitoring_component.py","boot_host.py","index_queue.py","index_coordinator.py","review_store.py","review_policy.py","post_deployment.py","apt_prefetch.py","packaging_cache.py","image_reuse.py","ui_policy.py","ui_publication.py","ui-test.Dockerfile","compiler.Dockerfile"):
            (self.source/name).write_bytes(b"# fixture module\n")
        (self.source/"scheduler-defaults.json").write_text("{}")
        index_source=self.v3/"plugins/abilities/Administrator/code_index"
        index_source.mkdir(parents=True)
        (index_source/"adapter.py").write_bytes(b"# fixture protected parser\n")
        (index_source/"maintenance.py").write_bytes(b"# fixture protected ledger\n")
        self.fake = FakePrivileges()
        self.patches = [patch.object(installer.shutil, "disk_usage", return_value=types.SimpleNamespace(free=5*1024*1024*1024)),
                        patch.object(installer, "PRIVILEGES", self.fake),
                        patch.object(installer, "V3", self.v3), patch.object(installer, "SOURCE", self.source),
                        patch.object(installer, "SERVICE", self.v3 / "host-build-service"),
                        patch.object(installer, "JOBS", self.v3 / "build-jobs"),
                        patch.object(installer, "ARCHIVES", self.v3 / "rollback/administrator")]
        for item in self.patches: item.start()

    def tearDown(self):
        for item in reversed(self.patches): item.stop()
        self.temp.cleanup()

    def test_install_exact_protected_code_and_repeat(self):
        result = installer.install()
        service = self.v3 / "host-build-service"
        self.assertEqual(self.host, (service / "host.py").read_bytes())
        self.assertEqual(hashlib.sha256(self.host).hexdigest(), result["host_sha256"])
        self.assertEqual(0o755, service.stat().st_mode & 0o777)
        self.assertEqual(0o644, (service / "host.py").stat().st_mode & 0o777)
        self.assertEqual(0o600, (service / "installed.json").stat().st_mode & 0o777)
        self.assertEqual(0, self.fake.owner((service / "host.py").stat()))
        self.assertEqual(hashlib.sha256(b"# fixture protected parser\n").hexdigest(),result["scanner_sha256"])
        self.assertEqual(0,self.fake.owner((service/"index-scanner.py").stat()))
        self.assertEqual(hashlib.sha256(b"# fixture protected performance\n").hexdigest(), result["performance_sha256"])
        self.assertEqual(0o644,(service/"performance.py").stat().st_mode & 0o777)
        self.assertEqual(0,self.fake.owner((service/"performance.py").stat()))
        for name in ("ownership.py","scheduler.py","resources.py","resource_control.py","docker_retention.py","docker_guard_dispatch.py","executor.py","engine_components.py","source_status.py","monitoring_component.py","boot_host.py","index_queue.py","index_coordinator.py","review_store.py","review_policy.py","post_deployment.py","apt_prefetch.py","packaging_cache.py","image_reuse.py","ui_policy.py","ui_publication.py","ui-test.Dockerfile","compiler.Dockerfile","scheduler-defaults.json"):
            self.assertEqual(hashlib.sha256((self.source/name).read_bytes()).hexdigest(),result["modules"][name])
            self.assertEqual((self.source/name).read_bytes(),(service/name).read_bytes())
        self.assertEqual(result, installer.install())

    def test_reviewed_tree_install_is_confined_and_protects_parser(self):
        key="a"*64
        task=self.v3/"build-jobs/coordination/trees"/key/"v3"
        task.mkdir(parents=True)
        import shutil
        shutil.copytree(self.source,task/"build-service")
        shutil.copytree(self.v3/"plugins/abilities/Administrator/code_index",task/"plugins/abilities/Administrator/code_index")
        result=installer.install(key)
        self.assertIn("scanner_sha256",result)
        for invalid in ("../escape","a"*63,"z"*64):
            with self.assertRaises(ValueError):installer.install(invalid)

    def test_nonroot_and_precreated_unowned_service_rejected(self):
        self.fake.euid = 1000
        with self.assertRaises(PermissionError): installer.install()
        self.fake.euid = 0
        (self.v3 / "host-build-service").mkdir()
        with self.assertRaisesRegex(PermissionError, "not root-owned"): installer.install()

    def test_symlink_and_hardlink_sources_or_destinations_rejected(self):
        (self.source / "host.py").unlink()
        outside = self.root / "private"
        outside.write_bytes(b"PRIVATE")
        (self.source / "host.py").symlink_to(outside)
        with self.assertRaises(OSError): installer.install()
        (self.source / "host.py").unlink()
        os.link(outside, self.source / "host.py")
        with self.assertRaisesRegex(ValueError, "single-link"): installer.install()
        (self.source / "host.py").unlink()
        (self.source / "host.py").write_bytes(self.host)
        installer.install()
        service = self.v3 / "host-build-service"
        (service / "host.py").unlink()
        (service / "host.py").symlink_to(outside)
        with self.assertRaises(PermissionError): installer.install()
        self.assertEqual(b"PRIVATE", outside.read_bytes())

    def test_component_symlink_directory_is_not_followed(self):
        outside = self.root / "outside"
        outside.mkdir()
        (self.root / "link").symlink_to(outside, target_is_directory=True)
        with self.assertRaises(OSError): installer.directory(self.root / "link")

    def artifact_fixture(self):
        jobs = self.v3 / "build-jobs"
        jobs.mkdir(mode=0o700)
        job = jobs / str(uuid.uuid4())
        job.mkdir(mode=0o700)
        (job / "rollback-images.tar").write_bytes(b"image export fixture")
        config = {role: {"Image": "sha256:" + "a" * 64, "Config": {"Env": ["TOKEN=PRIVATE-FIXTURE"]}, "HostConfig": {}} for role in ("api", "loop")}
        (job / "container-config.json").write_text(json.dumps(config))
        for name in ("rollback-images.tar", "container-config.json"): os.chmod(job / name, 0o600)
        return job

    def test_root_archives_only_two_private_artifacts_and_deduplicates(self):
        job = self.artifact_fixture()
        (job / "session.db").write_bytes(b"SQLite format 3\0PRIVATE")
        result = installer.archive_rollbacks()
        self.assertEqual({"copied": 2, "failed": 0}, result)
        protected = self.v3 / "rollback/administrator" / job.name
        self.assertEqual({"rollback-images.tar", "container-config.json", "manifest.json"}, {p.name for p in protected.iterdir()})
        self.assertEqual(0o700, protected.stat().st_mode & 0o777)
        self.assertEqual(0o600, (protected / "container-config.json").stat().st_mode & 0o777)
        self.assertEqual(0, self.fake.owner((protected / "rollback-images.tar").stat()))
        self.assertNotIn("PRIVATE-FIXTURE", (protected / "manifest.json").read_text())
        self.assertEqual({"copied": 0, "failed": 0}, installer.archive_rollbacks())

    def test_archive_rejects_symlink_hardlink_database_and_disk_exhaustion(self):
        job = self.artifact_fixture()
        source_fd = installer.directory(job)
        destination = self.root / "destination"
        destination.mkdir()
        destination_fd = installer.directory(destination)
        try:
            with patch.object(installer.shutil, "disk_usage", return_value=types.SimpleNamespace(free=0)), self.assertRaisesRegex(ValueError, "space"):
                installer.copy_artifact(source_fd, destination_fd, "rollback-images.tar", 1000)
            (job / "rollback-images.tar").write_bytes(b"SQLite format 3\0PRIVATE")
            with self.assertRaisesRegex(ValueError, "database"):
                installer.copy_artifact(source_fd, destination_fd, "rollback-images.tar", 1000)
            (job / "rollback-images.tar").unlink()
            (job / "rollback-images.tar").symlink_to(job / "container-config.json")
            with self.assertRaises(OSError): installer.copy_artifact(source_fd, destination_fd, "rollback-images.tar", 1000)
            (job / "rollback-images.tar").unlink()
            os.link(job / "container-config.json", job / "rollback-images.tar")
            with self.assertRaises(PermissionError): installer.copy_artifact(source_fd, destination_fd, "rollback-images.tar", 1000)
            self.assertEqual([], list(destination.iterdir()))
        finally:
            os.close(source_fd); os.close(destination_fd)

    def test_snapshot_is_fixed_source_without_runtime_data(self):
        src = self.v3 / "src"
        src.mkdir()
        (src / "main.rs").write_text("fn main() {}")
        cache = src / "__pycache__"
        cache.mkdir()
        (cache / "generated.pyc").write_bytes(b"bytecode")
        (src / "generated.pyo").write_bytes(b"bytecode")
        gates = self.v3 / "data/config/gates"
        gates.mkdir(parents=True)
        (gates / "message_visibility_v1.json").write_text("{}")
        context = installer.snapshot_context(("v3/src",))
        self.assertGreater(len(context), 0)
        with installer.tarfile.open(fileobj=installer.io.BytesIO(context)) as archive:
            names = [member.name for member in archive]
        self.assertIn("v3/src/main.rs", names)
        self.assertFalse(any("__pycache__" in name or name.endswith((".pyc", ".pyo")) for name in names))
        for invalid in ("v3/data/session.db", "v3/data/config/gates/message_visibility_v1.json",
                        "v3/workspaces/source", "v3/../data"):
            with self.assertRaises(ValueError): installer.snapshot_context((invalid,))
        (src / "secret.db").write_bytes(b"SQLite format 3")
        with self.assertRaisesRegex(ValueError, "database"): installer.snapshot_context(("v3/src",))

    def test_snapshot_skips_optional_root_files_until_first_promoted(self):
        src = self.v3 / "src"
        src.mkdir()
        (src / "main.rs").write_text("fn main() {}")
        context = installer.snapshot_context(("v3/src", "v3/live-files.json", "v3/rustfmt.toml"))
        with installer.tarfile.open(fileobj=installer.io.BytesIO(context)) as archive:
            names = [member.name for member in archive]
        self.assertEqual(names, ["v3/src/main.rs"])
        (self.v3 / "rustfmt.toml").write_text("max_width = 100\n")
        context = installer.snapshot_context(("v3/src", "v3/rustfmt.toml"))
        with installer.tarfile.open(fileobj=installer.io.BytesIO(context)) as archive:
            self.assertIn("v3/rustfmt.toml", [member.name for member in archive])

    def test_inspection_snapshot_backups_committed_wal_and_keeps_exact_scope(self):
        admin = self.v3 / "data/user_data/admin"
        agent = self.v3 / "data/agent_data/shared_default"
        admin.mkdir(parents=True)
        agent.mkdir(parents=True)
        live = sqlite3.connect(admin / "admin.db")
        try:
            live.execute("PRAGMA journal_mode=WAL")
            live.execute("PRAGMA wal_autocheckpoint=0")
            live.execute("CREATE TABLE docs (key TEXT, value BLOB)")
            live.execute("INSERT INTO docs VALUES (?,?)", ("wal-only", b"plaintext fixture"))
            live.commit()
            with sqlite3.connect(agent / "shared_default.db") as db:
                db.execute("CREATE TABLE settings (value TEXT)")
                db.execute("INSERT INTO settings VALUES ('agent fixture')")
            (admin / "notes.txt").write_text("all directory files included")
            blob = installer.snapshot_context(installer.INSPECTION_CONTEXT)
            with installer.tarfile.open(fileobj=installer.io.BytesIO(blob)) as archive:
                names = archive.getnames()
                self.assertIn("v3/data/user_data/admin/notes.txt", names)
                self.assertFalse(any(name.endswith(("-wal", "-shm")) for name in names))
                for member, table, expected in (
                    ("v3/data/user_data/admin/admin.db", "docs", "wal-only"),
                    ("v3/data/agent_data/shared_default/shared_default.db", "settings", "agent fixture")):
                    backup = self.root / (table + ".db")
                    backup.write_bytes(archive.extractfile(member).read())
                    with sqlite3.connect(backup.as_uri() + "?mode=ro&immutable=1", uri=True) as db:
                        self.assertEqual("ok", db.execute("PRAGMA integrity_check").fetchone()[0])
                        self.assertEqual(expected, db.execute("SELECT * FROM " + table).fetchone()[0])
                    self.assertEqual(0o440, archive.getmember(member).mode)
            for denied in ("v3/data", "v3/data/user_data", "v3/data/user_data/other", "v3/data/agent_data/other", "v3/data/encryption.key", "v3/data/runtime.sqlite"):
                with self.assertRaises(ValueError): installer.snapshot_context((denied,))
        finally:
            live.close()

    def test_inspection_rejects_key_and_database_links(self):
        admin = self.v3 / "data/user_data/admin"
        admin.mkdir(parents=True)
        (admin / "encryption.key").write_bytes(b"secret")
        with self.assertRaisesRegex(ValueError, "encryption key"):
            installer.snapshot_context((installer.INSPECTION_CONTEXT[0],))
        (admin / "encryption.key").unlink()
        outside = self.root / "outside.db"
        with sqlite3.connect(outside) as db: db.execute("CREATE TABLE t(v)")
        (admin / "admin.db").symlink_to(outside)
        with self.assertRaisesRegex(ValueError, "single-link"):
            installer.snapshot_context((installer.INSPECTION_CONTEXT[0],))
        (admin / "admin.db").unlink()
        os.link(outside, admin / "admin.db")
        with self.assertRaisesRegex(ValueError, "single-link"):
            installer.snapshot_context((installer.INSPECTION_CONTEXT[0],))

    def test_snapshot_maps_standalone_and_shared_roots_without_symlinks(self):
        shared = self.root / "shared-v2"
        css = shared / "ui/shared/css"
        css.mkdir(parents=True)
        (css / "index.css").write_text("body {}")
        (self.v3 / "src").mkdir()
        (self.v3 / "src/main.rs").write_text("fn main() {}")
        with patch.object(installer, "SHARED_SOURCE", shared):
            blob = installer.snapshot_context(("v3/src", "ui/shared/css/index.css"))
            with installer.tarfile.open(fileobj=installer.io.BytesIO(blob)) as archive:
                self.assertEqual(b"body {}", archive.extractfile("ui/shared/css/index.css").read())
                self.assertEqual(b"fn main() {}", archive.extractfile("v3/src/main.rs").read())
            (css / "index.css").unlink()
            (css / "index.css").symlink_to(self.v3 / "src/main.rs")
            with self.assertRaises(OSError):
                installer.snapshot_context(("ui/shared/css/index.css",))

    def test_snapshot_protocol_checks_peer_before_reading_source(self):
        stopped = threading.Event()
        class Connection:
            def __init__(self): self.output = b""
            def __enter__(self): return self
            def __exit__(self, *args): pass
            def settimeout(self, value): pass
            def getsockopt(self, *args): return struct.pack("3i", 1, 1101, 1100)
            def recv(self, count): raise AssertionError("unauthorized peer input must not be read")
            def sendall(self, data): self.output += data
        conn = Connection()
        class Listener:
            def settimeout(self, value): pass
            def accept(self): stopped.set(); return conn, None
        with patch.object(installer, "snapshot_context", side_effect=AssertionError("unauthorized snapshot")):
            installer.snapshot_server(Listener(), ("v3/src",), stopped)
        self.assertEqual(struct.pack("!Q", 0), conn.output)

    def test_privilege_drop_groups_before_ids_no_new_privs_fixed_exec(self):
        calls = []
        libc = Mock()
        libc.prctl.side_effect = lambda *args: calls.append(("no_new_privs", args)) or 0
        def record(name): return lambda *args: calls.append((name, args))
        with patch.object(installer.pwd, "getpwnam", return_value=types.SimpleNamespace(pw_name="ar",pw_uid=1000, pw_gid=1000)), \
             patch.dict(installer.os.environ,{"WEBAGENT_BUILD_CONFIG":"/etc/fixture.json"}), patch.object(installer.os,"getgroups",return_value=[1000]), patch.object(installer.os,"setgroups",record("supplementary")), patch.object(installer.os, "initgroups", record("groups")), patch.object(installer.os, "setgid", record("gid")), \
             patch.object(installer.os, "setuid", record("uid")), patch.object(installer.os, "set_inheritable", record("inheritable")), \
             patch.object(installer.os, "execve", record("exec")), patch.object(installer.ctypes, "CDLL", return_value=libc):
            installer._drop_and_exec(42, 43, 44)
        self.assertEqual(["groups","supplementary","gid","uid","no_new_privs"], [name for name,_ in calls[:5]])
        command = calls[-1][1]
        self.assertEqual("/usr/bin/python3", command[0])
        self.assertIn("/proc/self/fd/42", command[1])
        self.assertEqual({"PATH","HOME","LANG","WEBAGENT_BUILD_CONFIG","WEBAGENT_BUILD_MODULE_ROOT"},set(command[2]))


if __name__ == "__main__": unittest.main()

class RuntimeSocketTests(unittest.TestCase):
    def test_restart_removes_only_runtime_socket_and_rejects_replaced_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);runtime=root/"runtime";runtime.mkdir()
            protected=root/"protected";protected.mkdir()
            (protected/"snapshot.sock").write_text("untouched protected file")
            listener=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)
            listener.bind(str(runtime/"snapshot.sock"))
            fd=os.open(runtime,os.O_RDONLY|os.O_DIRECTORY)
            try:
                installer.remove_stale_socket(fd,"snapshot.sock",os.getuid())
                self.assertFalse((runtime/"snapshot.sock").exists())
                self.assertEqual((protected/"snapshot.sock").read_text(),"untouched protected file")
                (runtime/"snapshot.sock").write_text("replaced")
                with self.assertRaises(PermissionError):installer.remove_stale_socket(fd,"snapshot.sock",os.getuid())
            finally:os.close(fd);listener.close()

class WorkspaceInitializationTests(unittest.TestCase):
    def test_canonical_private_workspace_is_created_once_and_scope_escapes_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);privileges=FakePrivileges()
            with patch.object(installer,"PRIVILEGES",privileges),patch.dict(installer.CONFIG,{"workspace_root":str(root),"worker_uid":os.getuid(),"app_gid":os.getgid()}):
                sid=str(uuid.uuid4())
                result=installer.workspace_directory(sid);self.assertTrue(result["created"])
                self.assertEqual((root/sid).stat().st_mode&0o777,0o770)
                self.assertFalse(installer.workspace_directory(sid)["created"])
                with self.assertRaises(ValueError):installer.workspace_directory("../outside")
                other=str(uuid.uuid4());(root/other).symlink_to("/etc",target_is_directory=True)
                with self.assertRaises(OSError):installer.workspace_directory(other)
                with patch.dict(installer.CONFIG,{"worker_uid":os.getuid()+1}):
                    with self.assertRaises(PermissionError):installer.workspace_directory(sid)


class GateSecurityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec=importlib.util.spec_from_file_location('gate_lifecycle_tests',PATH.with_name('lifecycle.py'))
        cls.lifecycle=importlib.util.module_from_spec(spec);spec.loader.exec_module(cls.lifecycle)
    def gate(self,root):
        return {'Id':'1'*64,'Image':'sha256:'+'2'*64,'Config':{'User':'1102:1100','Image':'sha256:'+'2'*64,'Hostname':'old','Labels':{},'Env':['V3_GATE_SOCKET=/run/v3/gate/gate.sock']},
            'HostConfig':{'ReadonlyRootfs':True,'NetworkMode':'none','Privileged':False,'CapDrop':['ALL'],'CapAdd':None,'Devices':[],'DeviceRequests':None,'SecurityOpt':['no-new-privileges:true'],'Memory':64*1024**2,'PidsLimit':32,'NanoCpus':250000000},
            'Mounts':[{'Type':'bind','Source':str(root/'gate-runtime'),'Destination':'/run/v3/gate','RW':True}], 'State':{'Status':'running','Health':{'Status':'healthy'}}}
    def test_gate_isolation_refuses_each_authority_expansion(self):
        import copy
        root=Path('/fixture');before=self.gate(root);self.lifecycle.gate_boundary(before,root)
        changes=[('ReadonlyRootfs',False),('NetworkMode','host'),('Privileged',True),('CapAdd',['SYS_ADMIN']),('Devices',[{}]),('DeviceRequests',[{}]),('CapDrop',[]),('SecurityOpt',[]),('Memory',0),('PidsLimit',0),('NanoCpus',0)]
        for field,value in changes:
            with self.subTest(field=field):
                changed=copy.deepcopy(before);changed['HostConfig'][field]=value
                with self.assertRaises(ValueError):self.lifecycle.gate_boundary(changed,root)
        for change in ('identity','mount'):
            changed=copy.deepcopy(before)
            if change=='identity':changed['Config']['User']='0:0'
            else:changed['Mounts'].append({'Type':'bind','Source':'/','Destination':'/host','RW':True})
            with self.assertRaises(ValueError):self.lifecycle.gate_boundary(changed,root)
    def test_replacement_preserves_captured_configuration(self):
        import copy
        before=self.gate(Path('/fixture'));original=copy.deepcopy(before)
        result=self.lifecycle.gate_container_config(before,'sha256:'+'3'*64,'receipt')
        self.assertEqual(result['HostConfig'],before['HostConfig']);self.assertEqual(result['Env'],before['Config']['Env']);self.assertEqual(before,original)
        with self.assertRaises(ValueError):self.lifecycle.gate_container_config(before,'mutable:latest','receipt')
    def test_gate_refresh_requires_root(self):
        with patch.object(self.lifecycle.os,'geteuid',return_value=1000):
            with self.assertRaises(PermissionError):self.lifecycle.gate_security('/unread','task-fixture','a'*36)
    def workflow(self,fail_health=False):
        import copy,io,tarfile
        life=self.lifecycle
        with tempfile.TemporaryDirectory()as td:
            root=Path(td);old=self.gate(root);gate_name='gate-fixture';builder_prefix='buildx_buildkit_webagent-gate-security-';rid='a'*8+'-'+ 'a'*4+'-'+ 'a'*4+'-'+ 'a'*4+'-'+ 'a'*12
            config={'service_dir':str(PATH.parent),'install_root':str(root/'protected/build-service'),'checkout_root':str(root),'gate_container':gate_name,'api_container':'api-fixture','loop_container':'loop-fixture'}
            states={gate_name:copy.deepcopy(old),'api-fixture':{'Id':'api-id'},'loop-fixture':{'Id':'loop-id'}};events=[];pending={};new_image='sha256:'+'3'*64
            def find(name):
                return states.get(name)or next((v for v in states.values()if v.get('Id')==name),None)
            def output(argv,**kwargs):
                events.append(list(argv))
                if argv[:2]==['docker','inspect']:
                    v=find(argv[2]);assert v is not None,argv;return json.dumps([v]).encode()
                if argv[:3]==['docker','image','inspect']:return json.dumps([{'Id':'sha256:'+'4'*64}]).encode()
                if argv[:3]==['docker','buildx','inspect']:
                    states[builder_prefix+rid+'0']={'Id':'builder-id','Image':'sha256:'+'4'*64,'HostConfig':{'Memory':2*1024**3,'MemorySwap':2*1024**3}};return b''
                if argv[:2]==['docker','create']:return b'stage-id'
                if argv[:2]==['docker','cp']:
                    buf=io.BytesIO();data=b'fixed gate source\n'
                    with tarfile.open(fileobj=buf,mode='w')as tar:
                        m=tarfile.TarInfo('input');m.size=len(data);tar.addfile(m,io.BytesIO(data))
                    return buf.getvalue()
                if argv[:2]==['docker','run']and '-c'in argv:return json.dumps({p:hashlib.sha256(b'fixed gate source\n').hexdigest()for p in life.GATE_INPUTS}).encode()
                if argv[:2]==['docker','stop']:find(argv[2])['State']['Status']='exited'
                if argv[:2]==['docker','start']:find(argv[2])['State']['Status']='running'
                if argv[:2]==['docker','rename']:
                    v=find(argv[2]);key=next(k for k,x in states.items()if x is v);del states[key];states[argv[3]]=v
                return b''
            def run(argv,**kwargs):
                if argv[:2]==['docker','inspect']:
                    v=find(argv[2]);return types.SimpleNamespace(returncode=0 if v else 1,stdout=json.dumps([v]).encode()if v else b'')
                self.assertEqual(argv[:3],['docker','buildx','build']);Path(argv[argv.index('--iidfile')+1]).write_text(new_image);return types.SimpleNamespace(returncode=0)
            def request(conn,method,url,body,headers):pending.update(json.loads(body))
            def response(conn):
                candidate={'Id':'candidate-id','Image':new_image,'Config':{k:v for k,v in pending.items()if k!='HostConfig'},'HostConfig':pending['HostConfig'],'Mounts':copy.deepcopy(old['Mounts']),'State':{'Status':'created','Health':{'Status':'unhealthy'if fail_health else'healthy'}}};states[gate_name]=candidate
                return types.SimpleNamespace(status=201,read=lambda:json.dumps({'Id':'candidate-id'}).encode())
            operator=Mock(return_value={'busy':False,'queued':0,'drained':True,'task_id':'task-fixture'})
            import http.client
            original_read=Path.read_text
            with patch.object(Path,'read_text',lambda p,*a,**kw:'MemAvailable: 33554432 kB\n'if str(p)=='/proc/meminfo'else original_read(p,*a,**kw)),patch.object(life,'gate_builder_limits',return_value={'memory_max':2*1024**3,'swap_max':0,'oom':0,'oom_kill':0}),patch.object(life.os,'geteuid',return_value=0),patch.object(life.cfg,'load',return_value=config),patch.object(life,'secure_dirs',side_effect=lambda p:p.mkdir(parents=True,exist_ok=True)),patch.object(life,'module',side_effect=lambda name,path:types.SimpleNamespace(operator=operator)if path.name=='reconfigure.py'else types.SimpleNamespace(BUILDKIT='pinned@sha256:'+'4'*64)),patch.object(life.subprocess,'check_output',side_effect=output),patch.object(life.subprocess,'run',side_effect=run),patch.object(life.time,'sleep'),patch('shutil.disk_usage',return_value=types.SimpleNamespace(free=32*1024**3)),patch.object(http.client.HTTPConnection,'request',request),patch.object(http.client.HTTPConnection,'getresponse',response):
                if fail_health:
                    with self.assertRaisesRegex(RuntimeError,'health'):life.gate_security('/protected','task-fixture',rid)
                else:
                    result=life.gate_security('/protected','task-fixture',rid);self.assertEqual(result['status'],'completed')
                    replay=life.gate_security('/protected','task-fixture',rid);self.assertTrue(replay['replayed'])
            receipt=json.loads((root/'protected/gate-security'/rid/'receipt.json').read_bytes())
            self.assertEqual(receipt['status'],'rolled_back'if fail_health else'completed');self.assertEqual(states['api-fixture']['Id'],'api-id');self.assertEqual(states['loop-fixture']['Id'],'loop-id')
            if fail_health:self.assertEqual(states[gate_name]['Id'],old['Id']);self.assertTrue(any('failed-gate-security' in n for n in states))
            else:self.assertEqual(states[gate_name]['Image'],new_image);self.assertTrue(any('-gate-security-'in n and n.startswith(gate_name)for n in states))
            self.assertTrue(any(e[:3]==['docker','buildx','rm']for e in events))
    def test_gate_only_activation_and_idempotent_receipt(self):self.workflow()
    def test_unhealthy_candidate_restores_exact_predecessor(self):self.workflow(fail_health=True)

    def test_builder_kernel_limits_fail_closed(self):
        identity='a'*64;scope='/sys/fs/cgroup/fixture/docker-'+identity+'.scope'
        values={'/proc/99/cgroup':'0::/fixture/docker-'+identity+'.scope/init\n',scope+'/memory.max':str(2*1024**3),scope+'/memory.swap.max':'0',scope+'/memory.events':'oom 0\noom_kill 0\n'}
        info={'Id':identity,'State':{'Pid':99}}
        with patch.object(Path,'read_text',lambda p:values[str(p)]):
            self.assertEqual(self.lifecycle.gate_builder_limits(info)['swap_max'],0)
            self.assertEqual(self.lifecycle.gate_builder_limits(info)['scope'],scope)
            with self.assertRaisesRegex(RuntimeError,'exact Docker scope'):
                self.lifecycle.gate_builder_limits({'Id':'b'*64,'State':{'Pid':99}})
            values['/proc/99/cgroup']='0::/fixture/docker-'+identity+'.scope\n'
            self.assertEqual(self.lifecycle.gate_builder_limits(info)['scope'],scope)
            for key,bad in [('memory.max','max'),('memory.swap.max','1024'),('memory.events','oom 0\noom_kill 1\n')]:
                path=scope+'/'+key;before=values[path];values[path]=bad
                with self.assertRaises(RuntimeError):self.lifecycle.gate_builder_limits(info)
                values[path]=before
    def test_idle_drain_is_checked_before_docker_effects(self):
        life=self.lifecycle
        for state in [{'busy':True,'queued':0,'drained':True,'task_id':'task-fixture'},{'busy':False,'queued':1,'drained':True,'task_id':'task-fixture'},{'busy':False,'queued':0,'drained':False,'task_id':'task-fixture'},{'busy':False,'queued':0,'drained':True,'task_id':'foreign-task'}]:
            config={'service_dir':str(PATH.parent)}
            with patch.object(life.os,'geteuid',return_value=0),patch.object(life.cfg,'load',return_value=config),patch.object(life,'module',return_value=types.SimpleNamespace(operator=Mock(return_value=state))),patch.object(life.subprocess,'check_output')as docker:
                with self.assertRaisesRegex(RuntimeError,'idle broker drain'):life.gate_security('/protected','task-fixture','a'*36)
                docker.assert_not_called()
