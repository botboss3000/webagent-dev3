"""UI dependency fingerprints retain selective evidence and conservative unknowns."""
import importlib.util
from pathlib import Path
import unittest
spec=importlib.util.spec_from_file_location("ui_policy",Path(__file__).resolve().parents[1]/"build-service/ui_policy.py")
policy=importlib.util.module_from_spec(spec);spec.loader.exec_module(policy)


class UIDependencyPolicyTests(unittest.TestCase):
 def files(self,**values):return {"v3/ui/"+p:(420,text.encode())for p,text in values.items()}
 def test_static_dependency_change_selects_consumers_and_preserves_unrelated_files(self):
  files=self.files(**{"chat/a.js":"import './dep.js'; export const a=1;","chat/dep.js":"export const dependency=1;","chat/unrelated.js":"export const other=1;"})
  result=policy.dependencies(files,["v3/ui/chat/dep.js"])
  self.assertFalse(result["conservative"])
  self.assertIn("v3/ui/chat/a.js",result["affected"])
  self.assertNotIn("v3/ui/chat/unrelated.js",result["dependencies"])
 def test_dynamic_loading_uses_conservative_validation(self):
  files=self.files(**{"a.js":"import(variable)","b.js":"export const other=1;"})
  result=policy.dependencies(files,["v3/ui/b.js"])
  self.assertTrue(result["conservative"]);self.assertEqual(set(result["dependencies"]),set(files))
 def test_test_recipe_and_configuration_changes_change_fingerprint(self):
  files=self.files(**{"a.js":"export const a=1;","a.test.cjs":"const fixture=1;"})
  first=policy.dependencies(files,["v3/ui/a.js"])
  files["v3/ui/a.test.cjs"]=(420,b"const fixture=2;")
  self.assertNotEqual(first["sha256"],policy.dependencies(files,["v3/ui/a.js"])["sha256"])
  files["v3/config/chat.json"]=(420,b"{}")
  result=policy.dependencies(files,["v3/config/chat.json"])
  self.assertIn("v3/config/chat.json",result["dependencies"])
