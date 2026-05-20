//! Bridge [`SourcePoll`] to [`HostPollLineage`] minting in `baml-rt-core`.

use baml_rt_core::{HostPollLineage, PollLineageSeed, mint_host_poll_lineage};

use crate::{daemon::SourcePoll, model::SlackMessage};

pub fn poll_lineage_seed(poll: &SourcePoll) -> PollLineageSeed {
    PollLineageSeed {
        source_kind: poll.source_kind().as_str().to_string(),
        source_key: poll.source_key.clone(),
        source_cursor: poll.source_cursor().map(str::to_string),
        source_message_ts: poll
            .messages()
            .iter()
            .map(|m: &SlackMessage| m.ts.clone())
            .collect(),
    }
}

pub fn mint_poll_lineage(poll: &SourcePoll) -> Option<HostPollLineage> {
    mint_host_poll_lineage(&poll_lineage_seed(poll))
}
