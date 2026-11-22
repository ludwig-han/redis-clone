//! 공통 RESP 응답을 기존 바이트 형식 그대로 조립합니다.

pub(super) fn bulk_string(value: &[u8]) -> Vec<u8> {
    let mut reply = format!("${}\r\n", value.len()).into_bytes();
    reply.extend_from_slice(value);
    reply.extend_from_slice(b"\r\n");
    reply
}

pub(super) fn error_reply(message: &[u8]) -> Vec<u8> {
    let mut reply = vec![b'-'];
    reply.extend_from_slice(message);
    reply.extend_from_slice(b"\r\n");
    reply
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
