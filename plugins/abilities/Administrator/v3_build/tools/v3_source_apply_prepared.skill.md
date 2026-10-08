# v3_source_apply_prepared

Apply the exact reviewed diff saved by `v3_source_prepare` to its open isolated task. Supply `task_id`, a stable `request_id`, the returned `prepared_id`, and the same `expected_inputs_sha256`. This tool has no `patch` argument; never copy the preview diff into this call.

The broker checks the saved patch and current task digest, then records a durable apply receipt. This changes only the isolated task snapshot, not the live release. If the effect is uncertain, query `v3_source_receipt` with the same request ID before retrying.
