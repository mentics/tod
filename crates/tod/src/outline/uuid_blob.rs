//! UUID BLOB and timestamp helpers for outline persistence.

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Serialize a UUID for SQLite `BLOB` primary keys.
pub fn uuid_to_blob(id: Uuid) -> Vec<u8> {
    id.as_bytes().to_vec()
}

/// Parse a UUID from SQLite `BLOB`.
pub fn blob_to_uuid(bytes: &[u8]) -> anyhow::Result<Uuid> {
    let arr: [u8; 16] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("expected 16-byte UUID blob, got {} bytes", bytes.len()))?;
    Ok(Uuid::from_bytes(arr))
}

/// Parse a UUID from SQLite `BLOB` for `rusqlite` row mappers.
pub fn blob_to_uuid_sql(bytes: &[u8]) -> rusqlite::Result<Uuid> {
    blob_to_uuid(bytes).map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))
}

/// Current time as Unix milliseconds UTC.
pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Parse milliseconds since epoch to `DateTime<Utc>`.
pub fn ms_to_datetime(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or_else(Utc::now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_uuid_blob() {
        let id = Uuid::new_v4();
        let blob = uuid_to_blob(id);
        assert_eq!(blob_to_uuid(&blob).unwrap(), id);
    }
}
