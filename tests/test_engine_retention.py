"""Streamed inspection attestation and snapshot reply lifetime regressions."""
import gc,hashlib,importlib.util,json,socket,struct,tempfile,tracemalloc,unittest,weakref
from pathlib import Path
from unittest.mock import patch
import test_admin_build as fixtures
ROOT=Path(__file__).resolve().parents[1]
def module(name):
    """Load the exact owned candidate source without changing installed broker state."""
    spec=importlib.util.spec_from_file_location(name,ROOT/"build-service"/(name+".py"));value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value
release=module("release");installer=module("install")
class RetentionTests(unittest.TestCase):
    def test_inspection_streaming_preserves_source_equivalence_and_rejects_changes(self):
        private="v3/data/user_data/admin/fixture.db"
        base=fixtures.context(extra={private:"synthetic fixture","v3/docs/fixture.md":"before"})
        task=fixtures.context(extra={private:"synthetic fixture","v3/docs/fixture.md":"after"})
        actual,changed=release.candidate(base,task,base,fixtures.host.context_digest)
        expected=release.entries(task,fixtures.host.context_digest);expected.pop(private)
        self.assertEqual(release.entries(actual,fixtures.host.context_digest),expected);self.assertEqual(changed,["v3/docs/fixture.md"])
        for body,mode in (("changed fixture",0o644),("synthetic fixture",0o440)):
            files=release.entries(task,fixtures.host.context_digest);files[private]=(mode,body.encode());bad=release.archive(files,fixtures.host.context_digest)
            with self.assertRaisesRegex(ValueError,"inspection snapshot changed"):release.candidate(base,bad,base,fixtures.host.context_digest)
    def test_streamed_context_digest_matches_preserved_identity_semantics(self):
        blob=fixtures.context(extra={"v3/data/user_data/admin/fixture.db":"synthetic inspection","v3/docs/unicode.md":"fixture ☃","v3/docs/empty.md":""})
        legacy={name:(mode,len(body),hashlib.sha256(body).hexdigest())for name,(mode,body)in release.entries(blob,lambda _:None).items()}
        expected=(hashlib.sha256(fixtures.host.canonical(legacy)).hexdigest(),len(legacy))
        self.assertEqual(fixtures.host.context_digest(blob),expected)
    def test_large_inspection_is_attested_without_materializing_its_body(self):
        private="v3/data/user_data/admin/fixture.db";blob=fixtures.context(extra={private:"x"*(16*1024*1024)})
        tracemalloc.start()
        try:
            source,inspection=release.candidate_entries(blob,fixtures.host.context_digest);_,peak=tracemalloc.get_traced_memory()
        finally:tracemalloc.stop()
        self.assertNotIn(private,source);self.assertEqual(inspection[private][1],16*1024*1024);self.assertLess(peak,8*1024*1024)
    def test_snapshot_helper_drops_successful_and_failed_reply_before_next_accept(self):
        class Blob(bytearray):pass
        for fail in (False,True):
            with self.subTest(fail=fail):
                reference=[]
                class Stop:
                    done=False
                    def is_set(self):return self.done
                stopping=Stop()
                class Connection:
                    calls=0
                    def __enter__(self):return self
                    def __exit__(self,*args):return False
                    def settimeout(self,*args):pass
                    def getsockopt(self,*args):return struct.pack("3i",1,installer.CONFIG["service_uid"],0)
                    def recv(self,*args):return b'{"inspection":true}\n'
                    def sendall(self,value):
                        self.calls+=1
                        if fail and self.calls==2:raise OSError("synthetic send failure")
                connection=Connection()
                class Listener:
                    calls=0
                    def settimeout(self,*args):pass
                    def accept(self):
                        self.calls+=1
                        if self.calls==1:return connection,None
                        gc.collect();self.assert_released=reference[0]()is None;stopping.done=True;raise socket.timeout()
                listener=Listener()
                def snapshot(_):
                    blob=Blob(b"x"*1024);reference.append(weakref.ref(blob));return blob
                with patch.object(installer,"snapshot_context",side_effect=snapshot):installer.snapshot_server(listener,(),stopping)
                self.assertTrue(listener.assert_released)
if __name__=="__main__":unittest.main()
