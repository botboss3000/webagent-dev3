import importlib.util
from pathlib import Path
import unittest
spec=importlib.util.spec_from_file_location("pure_functions",Path(__file__).with_name("runtime.py"))
runtime=importlib.util.module_from_spec(spec)
spec.loader.exec_module(runtime)

class PureFunctions(unittest.TestCase):
    def test_saved_function_computes_json_without_ambient_authority(self):
        code='def total(args): return {"total": sum(args["items"]), "large": len(args["items"]) > 2}'
        self.assertEqual(runtime.invoke(code,{"items":[2,3,4]}),{"total":9,"large":True})
        self.assertEqual(runtime.invoke('def pick(args): return args["a"] if args["ok"] else 0',{"ok":False}),0)

    def test_code_execution_and_resource_escape_are_rejected(self):
        for code in ['import os\ndef f(args): return 1','def f(args): return open("/etc/passwd")',
                     'def f(args): return args.__class__','def f(args): return __import__("os")',
                     'def f(args): return [x for x in args]','def f(args): return 2 ** 999999',
                     'def f(args): return [[args] * 1000] * 1000',
                     'def f(args): return "x" * 999999999','def f(args): return "%999999999s" % args',
                     'def f(args):\n while True: pass','@print\ndef f(args): return 1',
                     'def f(args): return (lambda: 1)()']:
            with self.subTest(code=code),self.assertRaises((ValueError,TypeError)):
                runtime.invoke(code,{})

if __name__=='__main__':unittest.main()
