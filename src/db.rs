//! 공유 저장소의 타입과 만료 판단을 정의합니다.
//! 명령 전체의 원자성을 위해 lock은 command 모듈에서 잡고 유지합니다.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub(super) type Store = Arc<Mutex<HashMap<Vec<u8>, Entry>>>;

pub(super) struct Entry {
    pub(super) value: Vec<u8>,
    pub(super) expires_at: Option<Instant>,
}

pub(super) fn is_expired(entry: &Entry) -> bool {
    match entry.expires_at {
        Some(expires_at) => Instant::now() >= expires_at,
        None => false,
    }
}
