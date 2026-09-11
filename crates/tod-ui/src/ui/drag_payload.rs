//! Payload types shared between a drag source and drop target that live in
//! different view modules.

#[derive(Debug, Clone, Copy)]
pub struct ObligationDragPayload {
    pub obligation_id: uuid::Uuid,
}
