//! 공통 RESP 응답을 기존 바이트 형식 그대로 조립합니다.

pub(super) fn bulk_string(value: &[u8]) -> Vec<u8> {
    let mut reply = format!("${}\r\n", value.len()).into_bytes();
    reply.extend_from_slice(value);
    reply.extend_from_slice(b"\r\n");
    reply
}

/// RESP 배열: '*원소수\r\n' 뒤에 각 원소의 bulk string을 이어 붙입니다.
/// &[&[u8]]는 '바이트 묶음들을 가리키는 참조의 목록'입니다.
/// LRANGE는 저장된 원소를 소유권 이동/clone 없이 빌려서 이 함수에 전달할 수 있습니다.
pub(super) fn bulk_array(values: &[&[u8]]) -> Vec<u8> {
    let mut reply = format!("*{}\r\n", values.len()).into_bytes();
    for value in values {
        reply.extend_from_slice(&bulk_string(value));
    }
    reply
}

pub(super) fn error_reply(message: &[u8]) -> Vec<u8> {
    let mut reply = vec![b'-'];
    reply.extend_from_slice(message);
    reply.extend_from_slice(b"\r\n");
    reply
}

/// 값의 종류가 명령과 맞지 않을 때 사용하는 공통 응답입니다.
/// '-ERR ...'가 아니라 '-WRONGTYPE ...'이며, 앞의 '-'와 CRLF는 error_reply가 붙입니다.
pub(super) fn wrong_type_reply() -> Vec<u8> {
    error_reply(b"WRONGTYPE Operation against a key holding the wrong kind of value")
}

pub(super) fn unknown_command_reply(command: &[Vec<u8>]) -> Vec<u8> {
    let Some(name) = command.first() else {
        return Vec::new();
    };

    let mut message = format!("ERR unknown command '{}'", String::from_utf8_lossy(name));
    if command.len() > 1 {
        message.push_str(", with args beginning with: ");
        for argument in &command[1..] {
            // 진짜 Redis는 인자마다 뒤에 공백을 하나씩 붙입니다.
            message.push_str(&format!("'{}' ", String::from_utf8_lossy(argument)));
        }
    }

    error_reply(message.as_bytes())
}
