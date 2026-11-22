//! 공유 저장소의 타입과 만료 판단을 정의합니다.
//! 명령 전체의 원자성을 위해 lock은 command 모듈에서 잡고 유지합니다.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub(super) type Store = Arc<Mutex<HashMap<Vec<u8>, Entry>>>;

/// 한 키에 저장할 수 있는 값의 종류입니다.
/// Rust enum은 이름만 구분하는 것이 아니라, 각 경우에 실제 데이터를 담습니다.
/// Str과 List 중 하나만 선택되므로 한 키가 두 종류의 값을 동시에 가질 수 없습니다.
pub(super) enum Value {
    /// 기존 SET/GET의 값입니다. String 대신 Vec<u8>라서 UTF-8이 아닌 바이트도 저장합니다.
    Str(Vec<u8>),
    /// 바깥 VecDeque는 리스트 전체, 안쪽 Vec<u8> 하나는 리스트 원소 하나입니다.
    /// 예: [b"hello".to_vec(), b"world".to_vec()]처럼 원소마다 바이트 묶음을 소유합니다.
    /// VecDeque는 앞/뒤 삽입과 제거를 지원하는 표준 라이브러리의 양방향 큐입니다.
    List(VecDeque<Vec<u8>>),
}

/// TTL은 개별 리스트 원소가 아니라 키 전체에 적용됩니다.
/// 따라서 expires_at은 Value 안으로 옮기지 않고 기존처럼 Entry에 둡니다.
pub(super) struct Entry {
    pub(super) value: Value,
    pub(super) expires_at: Option<Instant>,
}

pub(super) fn is_expired(entry: &Entry) -> bool {
    match entry.expires_at {
        Some(expires_at) => Instant::now() >= expires_at,
        None => false,
    }
}
