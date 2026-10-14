"""Exact host framing and adoption metadata regressions."""
import importlib.util,json,sys,struct,tempfile,unittest,hashlib
from pathlib import Path
from unittest.mock import patch,MagicMock
ROOT=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT/"build-service"))
spec=importlib.util.spec_from_file_location("boot_transport_host",ROOT/"build-service/host.py")
h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
class TransportTests(unittest.TestCase):
 def test_exact_newline_and_actual_host_identity(self):
  with tempfile.TemporaryDirectory() as tmp:
   root=Path(tmp);home=root/"boot-supervisor";home.mkdir();(home/"ui.sock").touch()
   source=root/"service";source.mkdir();(source/"boot_host.py").write_text("fixture")
   value={"version":1,"mode":"live","revision":"fixture","required":True,"component_paused":False,"host_sha256":hashlib.sha256(b"fixture").hexdigest()}
   peer=MagicMock();peer.__enter__.return_value=peer;peer.getsockopt.return_value=struct.pack("3i",10,0,0)
   peer.makefile.return_value.readline.return_value=json.dumps({"status":"success","output":value}).encode()+bytes([10])
   with patch.dict(h.CONFIG,{"install_root":str(root/"build-service"),"service_dir":str(source)}),patch.object(h.socket,"socket",return_value=peer):
    self.assertEqual(h.Runner().boot_adoption(),value)
    self.assertEqual(peer.sendall.call_args.args[0],b'{"operation":"status"}'+bytes([10]))
    value["component_paused"]=True
    peer.makefile.return_value.readline.return_value=json.dumps({"status":"success","output":value}).encode()+bytes([10])
    with self.assertRaisesRegex(ValueError,"unavailable"):h.Runner().boot_adoption()
    value["required"]=False
    peer.makefile.return_value.readline.return_value=json.dumps({"status":"success","output":value}).encode()+bytes([10])
    self.assertEqual(h.Runner().boot_adoption(),value)
 def test_host_source_lane_is_fixed_and_installation_bound(self):
  source=(ROOT/"build-service/host.py").read_text()
  self.assertIn('"v3/build-service/boot_host.py"',source)
  self.assertIn('candidate[1]!=installed',source)
  self.assertIn('"v3/crates/boot-contracts/"',source)
