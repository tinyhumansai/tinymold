//! Refresh complete remote actions before preparing any local output bytes.

use super::*;
use std::cell::Cell;
use tinymold_wrap::RecordFile;

impl RemoteCache {
    /// A successful `GetActionResult` counts as a use on the server. Check the
    /// local record and all remote dependencies too, so an evicted object or
    /// a changed signing key falls back to the ordinary repair/upload path.
    pub(super) async fn refresh_actions(&self, records: &[RecordFile]) -> Result<HashSet<Digest>> {
        let actions = records
            .iter()
            .filter_map(|record| match record.kind {
                RecordKind::Action { key, .. } => Some((0, (key, &record.path))),
                _ => None,
            })
            .collect();
        let mut refreshed = HashSet::new();
        // One cache-wide outage costs at most the currently running group,
        // rather than a retry schedule for every queued action in the store.
        let unavailable = Cell::new(false);
        in_flight(
            actions,
            |(key, path)| {
                let unavailable = &unavailable;
                async move {
                    if unavailable.get() {
                        return (key, Ok(false));
                    }
                    let result = self.refresh_action(&key, path).await;
                    if result.as_ref().is_err_and(transient) {
                        unavailable.set(true);
                    }
                    (key, result)
                }
            },
            |(key, present)| {
                match present {
                    Ok(true) => {
                        refreshed.insert(key);
                    }
                    Ok(false) | Err(Error::Malformed { .. } | Error::Untrusted { .. }) => {}
                    // Refresh is optional: a stalled read must not prevent
                    // healthy writes from repairing or publishing entries.
                    Err(Error::Status { status, .. }) if status.code() == tonic::Code::NotFound => {
                    }
                    Err(error) if transient(&error) => {}
                    Err(error) => return Err(error),
                }
                Ok(())
            },
        )
        .await?;
        Ok(refreshed)
    }

    async fn refresh_action(&self, key: &Digest, path: &Path) -> Result<bool> {
        let Ok(text) = fs::read(path) else {
            return Ok(false);
        };
        let Ok(references) = tinymold_wrap::action_references(path) else {
            return Ok(false);
        };
        let key = naming::action_key(key);
        let Some(result) = retrying(|| self.client.get_action_result(key.clone())).await? else {
            return Ok(false);
        };
        let record = naming::blob(&text);
        if result.exit_code != 0 || find_file(&result, ACTION_FILE, &key)? != record {
            return Ok(false);
        }
        // Compare the deterministic signature rather than fetching output
        // bytes. Never re-sign or republish a result supplied by the remote.
        if let Some(signer) = self.signing_key() {
            let expected = naming::blob(&signer.sign(Kind::Action, &key, &record));
            if find_file(&result, SIGNATURE_FILE, &key)? != expected {
                return Ok(false);
            }
        } else {
            retrying(|| self.verify(Kind::Action, &key, &record, &result)).await?;
        }
        let expected: HashSet<_> = references.iter().map(Digest::to_hex).collect();
        let actual: HashSet<_> = result
            .output_files
            .iter()
            .filter(|file| file.path != ACTION_FILE && file.path != SIGNATURE_FILE)
            .map(|file| file.path.clone())
            .collect();
        if actual != expected || result.output_files.iter().any(|file| file.digest.is_none()) {
            return Ok(false);
        }
        let digests: Vec<_> = result
            .output_files
            .iter()
            .filter_map(|file| file.digest.clone())
            .collect();
        for batch in digests.chunks(FIND_BATCH) {
            if !retrying(|| self.client.find_missing(batch.to_vec()))
                .await?
                .is_empty()
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
