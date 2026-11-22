//! RESP 길이와 명령 인자에 공통으로 쓰는 기존 i64 변환입니다.

pub(super) fn parse_number(bytes: &[u8]) -> Option<i64> {
    std::str::from_utf8(bytes).ok()?.parse().ok() // from_utf8와 parse를 연달아 시도합니다. 어느 쪽이 실패해도 None을 돌려줍니다.
}

/// 새 List 명령의 정수 인자는 Redis처럼 '+1', '01', '-0'을 허용하지 않습니다.
/// i64로 읽은 뒤 표준 십진 표기와 비교합니다. 기존 S01~S05의 파서는 그대로 둡니다.
pub(super) fn parse_list_integer(bytes: &[u8]) -> Option<i64> {
    let value = parse_number(bytes)?;
    if value.to_string().as_bytes() == bytes {
        Some(value)
    } else {
        None
    }
}
