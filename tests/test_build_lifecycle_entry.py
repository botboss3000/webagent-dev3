"""Lifecycle installation resolves protected bootstrap without weakening fixed paths."""
import types
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec=importlib.util.spec_from_file_location("lifecycle_entry",Path(__file__).resolve().parents[1]/"build-service/lifecycle.py")
lifecycle=importlib.util.module_from_spec(spec);spec.loader.exec_module(lifecycle)

class LifecycleEntryTests(unittest.TestCase):
    def test_source_tree_uses_fixed_install_script(self):
        with tempfile.TemporaryDirectory()as temporary:
            root=Path(temporary);(root/"install.py").write_text("# reviewed installer\n");(root/"bootstrap.py").write_text("# installed alternative\n")
            self.assertEqual(lifecycle.installation_entry(root),root/"install.py")

    def test_installed_layout_uses_fixed_protected_bootstrap(self):
        with tempfile.TemporaryDirectory()as temporary:
            root=Path(temporary);(root/"bootstrap.py").write_text("# protected installer\n")
            self.assertEqual(lifecycle.installation_entry(root),root/"bootstrap.py")

    def test_missing_and_symlink_entries_are_rejected(self):
        with tempfile.TemporaryDirectory()as temporary:
            root=Path(temporary)
            with self.assertRaises(FileNotFoundError):lifecycle.installation_entry(root)
            (root/"bootstrap.py").write_text("# protected fixture\n");(root/"install.py").symlink_to(root/"bootstrap.py")
            with self.assertRaises(PermissionError):lifecycle.installation_entry(root)

class StaleTreeGuardTests(unittest.TestCase):
    """A tree cut before the installed release must not silently revert it."""
    def layout(self,base,tree,installed):
        import io,tarfile
        temporary=tempfile.TemporaryDirectory();self.addCleanup(temporary.cleanup);root=Path(temporary.name)
        folder=root/"tree";(folder/"v3/build-service").mkdir(parents=True);release=root/"release";release.mkdir()
        with tarfile.open(folder/"baseline.tar","w")as archive:
            for name,body in base.items():
                info=tarfile.TarInfo("v3/build-service/"+name);info.size=len(body);archive.addfile(info,io.BytesIO(body))
        for name,body in tree.items():(folder/"v3/build-service"/name).write_bytes(body)
        for name,body in installed.items():(release/name).write_bytes(body)
        return folder,release
    def test_tree_cut_before_the_installed_release_is_stale(self):
        tree,release=self.layout(base={"host.py":b"old","executor.py":b"old"},tree={"host.py":b"old","executor.py":b"mine"},
                                 installed={"host.py":b"theirs","executor.py":b"old"})
        self.assertEqual(lifecycle.stale_tree(tree,release),["host.py"])
    def test_tree_based_on_the_installed_release_passes(self):
        tree,release=self.layout(base={"host.py":b"theirs","install.py":b"i"},tree={"host.py":b"mine","install.py":b"i2"},
                                 installed={"host.py":b"theirs","bootstrap.py":b"i","index-maintenance.py":b"x"})
        self.assertEqual(lifecycle.stale_tree(tree,release),[])
    def test_tree_that_already_carries_the_installed_bytes_passes(self):
        tree,release=self.layout(base={"host.py":b"old"},tree={"host.py":b"theirs"},installed={"host.py":b"theirs"})
        self.assertEqual(lifecycle.stale_tree(tree,release),[])
    def test_install_refuses_unless_explicitly_overridden(self):
        source=(Path(__file__).resolve().parents[1]/"build-service/lifecycle.py").read_text()
        self.assertIn("--allow-stale-tree",source)
        self.assertIn('if stale and not args.allow_stale_tree:',source)

class BootPinTests(unittest.TestCase):
    """Broker installs repoint the boot host's reviewed-release drop-in instead of leaving it stale."""
    def layout(self,text=None):
        temporary=tempfile.TemporaryDirectory();self.addCleanup(temporary.cleanup);root=Path(temporary.name)
        install=root/"var/build-service";old=install/"releases"/("a"*64);new=install/"releases"/("b"*64)
        for folder in (old,new):folder.mkdir(parents=True);(folder/"boot_host.py").write_text("# reviewed boot host\n")
        units=root/"units";folder=units/"webagent-boot-development.service.d";folder.mkdir(parents=True)
        if text is not None:(folder/lifecycle.BOOT_PIN).write_text(text.format(old=old))
        value={"instance":"development","install_root":str(install),"service_dir":str(new)}
        return root,units,value,folder/lifecycle.BOOT_PIN,new
    def test_stale_pin_is_backed_up_rewritten_and_only_the_boot_host_restarted(self):
        root,units,value,path,new=self.layout("[Service]\nExecStart=\nExecStart=/usr/bin/python3 -B {old}/boot_host.py\n")
        before=path.read_text();calls=[]
        run=lambda state:lambda argv,check,**_:calls.append(argv) or types.SimpleNamespace(stdout=state+"\n")
        result=lifecycle.repoint_boot(lifecycle.boot_pin(value,units),root/"pin.previous",run("active"))
        self.assertEqual(path.read_text(),"[Service]\nExecStart=\nExecStart=/usr/bin/python3 -B "+str(new)+"/boot_host.py\n")
        self.assertEqual((root/"pin.previous").read_text(),before)
        unit="webagent-boot-development.service"
        self.assertEqual(calls,[["systemctl","daemon-reload"],["systemctl","is-active",unit],["systemctl","reset-failed",unit],["systemctl","restart",unit]])
        self.assertEqual(result["restarted"],unit)
        calls.clear();self.assertFalse(lifecycle.repoint_boot(lifecycle.boot_pin(value,units),root/"pin.previous",run("active"))["changed"])
        self.assertEqual(calls,[])
    def test_operator_stopped_boot_host_is_repointed_but_left_stopped(self):
        root,units,value,path,new=self.layout("[Service]\nExecStart=\nExecStart=/usr/bin/python3 -B {old}/boot_host.py\n");calls=[]
        result=lifecycle.repoint_boot(lifecycle.boot_pin(value,units),root/"pin.previous",lambda argv,check,**_:calls.append(argv) or types.SimpleNamespace(stdout="inactive\n"))
        self.assertIn(str(new)+"/boot_host.py",path.read_text());self.assertIsNone(result["restarted"])
        self.assertEqual(calls,[["systemctl","daemon-reload"],["systemctl","is-active","webagent-boot-development.service"]])
    def test_absent_drop_in_is_left_alone(self):
        _,units,value,path,_=self.layout()
        self.assertIsNone(lifecycle.boot_pin(value,units));self.assertFalse(path.exists())
    def test_unfamiliar_or_symlinked_drop_in_fails_closed(self):
        for text in ("[Service]\nExecStart=/usr/bin/python3 /tmp/boot_host.py\n","ExecStart={old}/boot_host.py\nExecStartPre={old}/boot_host.py\n"):
            _,units,value,_,_=self.layout(text)
            with self.assertRaises(ValueError):lifecycle.boot_pin(value,units)
        root,units,value,path,_=self.layout("ExecStart={old}/boot_host.py\n")
        real=root/"real.conf";path.rename(real);path.symlink_to(real)
        with self.assertRaises(PermissionError):lifecycle.boot_pin(value,units)
    def test_missing_installed_boot_host_never_rewrites_the_pin(self):
        root,units,value,path,new=self.layout("ExecStart={old}/boot_host.py\n");before=path.read_text()
        plan=lifecycle.boot_pin(value,units);(new/"boot_host.py").unlink()
        with self.assertRaises(FileNotFoundError):lifecycle.repoint_boot(plan,root/"pin.previous",lambda argv,check:None)
        self.assertEqual(path.read_text(),before);self.assertFalse((root/"pin.previous").exists())
