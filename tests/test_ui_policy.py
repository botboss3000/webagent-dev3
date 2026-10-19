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

 def test_unknown_in_other_component_does_not_taint_resolved_fixture(self):
  files=self.files(**{"chat/a.js":"export const a=1;","chat/a.test.mjs":"import test from 'node:test'; import './a.js';","other/loader.js":"import(variable)","other/x.js":"const x=1;"})
  first=policy.dependencies(files,["v3/ui/chat/a.js"])
  proof=first["test_inputs"]["v3/ui/chat/a.test.mjs"]
  self.assertFalse(proof["conservative"])
  files["v3/ui/other/x.js"]=(420,b"const x=2;")
  second=policy.dependencies(files,["v3/ui/chat/a.js"])
  self.assertEqual(proof["sha256"],second["test_inputs"]["v3/ui/chat/a.test.mjs"]["sha256"])
  files["v3/ui/chat/a.js"]=(420,b"export const a=2;")
  self.assertNotEqual(proof["sha256"],policy.dependencies(files,["v3/ui/chat/a.js"])["test_inputs"]["v3/ui/chat/a.test.mjs"]["sha256"])
 def test_unknown_fixture_binds_shared_inputs_modes_and_inventory(self):
  files=self.files(**{"chat/a.test.mjs":"import fs from 'node:fs';fs.readFileSync(variable);","x.js":"const x=1;"})
  files["ui/shared/style.css"]=(420,b"body{}")
  first=policy.dependencies(files,["v3/ui/x.js"])["test_inputs"]["v3/ui/chat/a.test.mjs"]
  self.assertTrue(first["conservative"]);self.assertIn("ui/shared/style.css",first["dependencies"])
  for change in [{"ui/shared/style.css":(420,b"body{color:red}")},{"v3/ui/new.json":(420,b"{}")},{"v3/ui/x.js":(448,b"const x=1;")}]:
   second=policy.dependencies({**files,**change},["v3/ui/x.js"])["test_inputs"]["v3/ui/chat/a.test.mjs"]
   self.assertNotEqual(first["sha256"],second["sha256"])
 def test_imported_unknown_dependency_keeps_fixture_conservative(self):
  files=self.files(**{"a.test.mjs":"import './helper.js';","helper.js":"fetch(variable)","other.js":"const a=1;"})
  result=policy.dependencies(files,["v3/ui/helper.js"])
  self.assertTrue(result["test_inputs"]["v3/ui/a.test.mjs"]["conservative"])
