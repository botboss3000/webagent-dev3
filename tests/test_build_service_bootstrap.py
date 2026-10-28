"""Portable deployment builds the mounts and identities consumed by launch.py."""
import importlib.util,tempfile,unittest,types
from pathlib import Path
from unittest.mock import patch
spec=importlib.util.spec_from_file_location("portable_deploy",Path(__file__).resolve().parents[1]/"build-service/deploy.py")
deploy=importlib.util.module_from_spec(spec);spec.loader.exec_module(deploy)
class BootstrapTests(unittest.TestCase):
 def test_first_start_mounts_source_runtime_and_configured_identity(self):
  with tempfile.TemporaryDirectory() as temporary:
   root=Path(temporary);(root/"config").mkdir()
   value=deploy.cfg.defaults(root);value.update(api_container="isolated-api",source_uid=2402)
   calls=[]
   def run(argv,**kwargs):
    calls.append(argv)
    return types.SimpleNamespace(returncode=1 if argv[:2]==["docker","inspect"] else 0)
   with patch.object(deploy.cfg,"load",return_value=value),patch.object(deploy.subprocess,"run",side_effect=run),patch.object(deploy,"initialize_state") as initialize,patch("sys.argv",["deploy","start"]):deploy.main()
   initialize.assert_called_once_with(root,value)
   launch=calls[-1]
   self.assertIn("type=bind,src="+str(root)+",dst=/live-v3,readonly",launch)
   self.assertIn("type=bind,src="+str(root)+",dst=/live-checkout",launch)
   self.assertIn("V3_SOURCE_OWNER_UID=2402",launch)
   self.assertIn("isolated-api",launch)

 def test_state_folders_are_created_without_seeding_and_reject_directory_symlink(self):
  import os
  with tempfile.TemporaryDirectory() as temporary:
   root=Path(temporary);(root/"config").mkdir()
   config={"app_uid":os.getuid(),"app_gid":os.getgid()}
   with patch.object(deploy.os,"geteuid",return_value=0):
    deploy.initialize_state(root,config)
    self.assertEqual((root/"data/config").stat().st_mode&0o777,0o770)
    # Configuration is live source; nothing is copied into the state root.
    self.assertEqual(list((root/"data/config").iterdir()),[])
    (root/"data/config/app-settings.json").write_text('{"preserved":true}')
    deploy.initialize_state(root,config)
    self.assertEqual((root/"data/config/app-settings.json").read_text(),'{"preserved":true}')
   import shutil
   shutil.rmtree(root/"data/config");(root/"data/config").symlink_to(root/"config",target_is_directory=True)
   with patch.object(deploy.os,"geteuid",return_value=0),self.assertRaises(OSError):deploy.initialize_state(root,config)
