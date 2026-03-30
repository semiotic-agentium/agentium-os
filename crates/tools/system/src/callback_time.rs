use tracing::warn;

pub fn callback_now_unix_ms(clock_event: &'static str) -> u64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as u64,
        Err(error) => {
            warn!(
                clock_event = clock_event,
                error = %error,
                "system callback clock skew detected; using zero unix timestamp"
            );
            0
        }
    }
}
