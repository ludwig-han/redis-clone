//! RESP 길이와 명령 인자에 공통으로 쓰는 기존 i64 변환입니다.

pub(super) fn parse_number(bytes: &[u8]) -> Option<i64> {
    std::str::from_utf8(bytes).ok()?.parse().ok() // from_utf8와 parse를 연달아 시도합니다. 어느 쪽이 실패해도 None을 돌려줍니다.
}
