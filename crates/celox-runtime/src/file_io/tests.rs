use super::*;
use num_bigint::BigUint;

#[test]
fn error_status_is_per_descriptor_and_cleared_by_success() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("errors");
    std::fs::write(&path, b"x").unwrap();
    let mut files = FileIo::default();
    let fd = files.open(&path, Some("r"));
    let other = files.open(&path, Some("r"));
    assert_eq!(files.error(fd), (0, String::new()));
    assert!(files.seek(fd, -1, 0).is_err());
    let error = files.error(fd);
    assert_ne!(error.0, 0);
    assert!(!error.1.is_empty());
    assert_eq!(files.error(fd), error);
    assert_eq!(files.read_byte(other).unwrap(), Some(b'x'));
    assert_eq!(files.error(fd), error);
    assert_eq!(files.error(other), (0, String::new()));
    assert_eq!(files.read_byte(fd).unwrap(), Some(b'x'));
    assert_eq!(files.read_byte(fd).unwrap(), None);
    assert_eq!(files.error(fd), (0, String::new()));
    assert!(files.write(fd, b"denied").is_err());
    assert_ne!(files.error(fd).0, 0);
    files.rewind(fd).unwrap();
    assert_eq!(files.error(fd), (0, String::new()));
    files.close(fd).unwrap();
    assert_ne!(files.error(fd).0, 0);
    let reopened = files.open(&path, Some("r"));
    assert_eq!(reopened, fd);
    assert_eq!(files.error(reopened), (0, String::new()));
    assert_eq!(files.open(directory.path().join("missing"), Some("r")), 0);
    assert_ne!(files.error(0).0, 0);
    assert!(!files.error(0).1.is_empty());
}

#[test]
fn byte_input_tracks_eof_pushback_and_logical_position() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input");
    std::fs::write(&path, [0, 255, b'Z']).unwrap();
    let mut files = FileIo::default();
    let fd = files.open(&path, Some("rb"));
    assert!(!files.eof(fd).unwrap());
    assert_eq!(files.tell(fd).unwrap(), 0);
    assert_eq!(files.read_byte(fd).unwrap(), Some(0));
    assert_eq!(files.read_byte(fd).unwrap(), Some(255));
    files.unread_byte(fd, b'Q').unwrap();
    assert_eq!(files.tell(fd).unwrap(), 1);
    assert!(files.unread_byte(fd, b'R').is_err());
    assert_eq!(files.read_byte(fd).unwrap(), Some(b'Q'));
    assert_eq!(files.tell(fd).unwrap(), 2);
    assert_eq!(files.read_byte(fd).unwrap(), Some(b'Z'));
    assert!(!files.eof(fd).unwrap());
    assert_eq!(files.read_byte(fd).unwrap(), None);
    assert!(files.eof(fd).unwrap());
    files.unread_byte(fd, b'X').unwrap();
    assert!(!files.eof(fd).unwrap());
    files.seek(fd, 0, 1).unwrap();
    assert_eq!(files.read_byte(fd).unwrap(), Some(b'Z'));
    assert_eq!(files.read_byte(fd).unwrap(), None);
    files.rewind(fd).unwrap();
    assert!(!files.eof(fd).unwrap());
    assert_eq!(files.tell(fd).unwrap(), 0);
    assert_eq!(files.read_byte(fd).unwrap(), Some(0));
    files.close(fd).unwrap();
    assert!(files.read_byte(fd).is_err());
    assert!(files.eof(fd).is_err());
    assert!(files.unread_byte(fd, 0).is_err());
    assert!(files.tell(fd).is_err());
    assert_eq!(std::fs::read(path).unwrap(), [0, 255, b'Z']);
}

#[test]
fn eof_remains_latched_when_another_descriptor_appends_data() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("growing");
    std::fs::write(&path, []).unwrap();
    let mut files = FileIo::default();
    let reader = files.open(&path, Some("r"));
    assert_eq!(files.read_byte(reader).unwrap(), None);
    let writer = files.open(&path, Some("a"));
    files.write(writer, b"new").unwrap();
    assert_eq!(files.read_byte(reader).unwrap(), None);
    assert!(files.eof(reader).unwrap());
    files.seek(reader, 0, 1).unwrap();
    assert!(!files.eof(reader).unwrap());
    assert_eq!(files.read_byte(reader).unwrap(), Some(b'n'));
}

#[test]
fn append_modes_open_at_eof_without_latching_eof_until_read() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("append-position");
    std::fs::write(&path, b"ab").unwrap();
    let mut files = FileIo::default();
    for mode in ["a", "ab", "a+", "a+b", "ab+"] {
        let fd = files.open(&path, Some(mode));
        assert_ne!(fd, 0, "{mode}");
        assert_eq!(files.tell(fd).unwrap(), 2, "{mode}");
        if mode.contains('+') {
            assert!(!files.eof(fd).unwrap(), "{mode}");
            assert_eq!(files.read_byte(fd).unwrap(), None, "{mode}");
            assert!(files.eof(fd).unwrap(), "{mode}");
            files.rewind(fd).unwrap();
            assert!(!files.eof(fd).unwrap(), "{mode}");
            assert_eq!(files.read_byte(fd).unwrap(), Some(b'a'), "{mode}");
        }
        files.close(fd).unwrap();
    }
    assert_eq!(std::fs::read(path).unwrap(), b"ab");
}

#[test]
fn position_errors_preserve_pushback_and_append_ignores_seek_for_writes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("append");
    std::fs::write(&path, b"ab").unwrap();
    let mut files = FileIo::default();
    let fd = files.open(&path, Some("a+b"));
    files.rewind(fd).unwrap();
    assert_eq!(files.read_byte(fd).unwrap(), Some(b'a'));
    files.unread_byte(fd, b'Q').unwrap();
    assert!(files.seek(fd, -1, 0).is_err());
    assert!(files.seek(fd, 0, 9).is_err());
    assert_eq!(files.read_byte(fd).unwrap(), Some(b'Q'));
    files.rewind(fd).unwrap();
    files.write(fd, b"c").unwrap();
    assert_eq!(files.tell(fd).unwrap(), 3);
    files.seek(fd, -1, 2).unwrap();
    assert_eq!(files.read_byte(fd).unwrap(), Some(b'c'));
    files.close(fd).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"abc");
}

#[test]
fn flush_validates_fanout_and_keeps_streams_open() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("output");
    let mut files = FileIo::default();
    let channel = files.open(&path, None);
    files.write(channel, b"a").unwrap();
    files.flush(Some(channel | 1)).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"a");
    assert!(files.flush(Some(channel | (1 << 30))).is_err());
    files.write(channel, b"b").unwrap();
    files.flush(None).unwrap();
    files.close(channel).unwrap();
    assert!(files.flush(Some(channel)).is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"ab");
    files.close(FD_FLAG | 1).unwrap();
    assert!(files.flush(Some(FD_FLAG | 1)).is_err());
    assert!(files.flush(Some(1)).is_err());
    // Flush-all skips closed handles.
    files.flush(None).unwrap();
}

#[test]
fn reads_and_positions_reject_mcds_and_write_only_handles() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("output");
    let mut files = FileIo::default();
    let mcd = files.open(&path, None);
    assert!(files.read_byte(mcd).is_err());
    assert!(files.unread_byte(mcd, 1).is_err());
    assert!(files.eof(mcd).is_err());
    assert!(files.tell(mcd).is_err());
    assert!(files.rewind(mcd).is_err());
    let fd = files.open(&path, Some("w"));
    assert!(files.read_byte(fd).is_err());
    assert!(files.unread_byte(fd, 1).is_err());
    assert!(files.eof(fd).is_err());
    assert_eq!(files.tell(fd).unwrap(), 0);
    assert!(files.read_byte(FD_FLAG | 1).is_err());
    files.close(FD_FLAG).unwrap();
    assert!(files.read_byte(FD_FLAG).is_err());
}

#[test]
fn descriptors_modes_channels_and_reuse() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    let mut files = FileIo::default();
    let m1 = files.open(&a, None);
    let m2 = files.open(&b, None);
    assert_eq!((m1, m2), (2, 4));
    assert!(files.write(m1 | m2 | 1, b"both\n").unwrap());
    files.close(m1 | m2).unwrap();
    assert_eq!(std::fs::read(&a).unwrap(), b"both\n");
    assert_eq!(std::fs::read(&b).unwrap(), b"both\n");
    assert!(files.write(m1, b"closed").is_err());
    assert_eq!(files.open(&b, None), m1);
    let fd = files.open(&a, Some("ab+"));
    assert_eq!(fd, 0x8000_0003);
    files.write(fd, b"appended").unwrap();
    files.close(fd).unwrap();
    assert_eq!(std::fs::read(&a).unwrap(), b"both\nappended");
    assert_eq!(files.open(dir.path().join("missing"), Some("r")), 0);
    assert_eq!(files.open(&a, Some("bad")), 0);
    assert!(files.write(0x8000_0000, b"stdin").is_err());
}

#[test]
fn stdout_selection_survives_mcd_errors_but_respects_closed_stdout() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("valid-channel.txt");
    let mut files = FileIo::default();
    let channel = files.open(&path, None);
    assert_ne!(channel, 0);
    let (stdout, result) = files.write_with_stdout(channel | 1 | (1 << 30), b"attempted");
    assert!(stdout);
    assert!(result.is_err());
    files.close(channel).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"attempted");
    files.close(1).unwrap();
    let (stdout, result) = files.write_with_stdout(1 | channel, b"closed");
    assert!(!stdout);
    assert!(result.is_err());
    let (stdout, result) = files.write_with_stdout(FD_FLAG | 1, b"closed");
    assert!(!stdout);
    assert!(result.is_err());
}

#[test]
fn standard_file_descriptors_cannot_be_closed_twice() {
    for descriptor in [FD_FLAG, FD_FLAG | 1, FD_FLAG | 2] {
        let mut files = FileIo::default();
        files.close(descriptor).unwrap();
        assert_eq!(
            files.close(descriptor).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(files.write(descriptor, b"").is_err());
    }
}

#[test]
fn stdout_fd_and_mcd_share_close_state_without_skipping_other_channels() {
    for (first, repeated) in [(1, FD_FLAG | 1), (FD_FLAG | 1, 1)] {
        let mut files = FileIo::default();
        files.close(first).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fanout.txt");
        let channel = files.open(&path, None);
        files.write(channel, b"kept").unwrap();
        assert!(files.close(repeated).is_err());
        // An invalid stdout component must not skip closing valid MCDs.
        assert!(files.close(channel | 1).is_err());
        assert!(files.write(channel, b"closed").is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"kept");
        // Closing stdout must not affect the other standard stream.
        files.close(FD_FLAG | 2).unwrap();
    }
}

#[test]
fn zero_mcd_is_invalid_even_for_an_empty_write() {
    let mut files = FileIo::default();
    assert_eq!(
        files.write(0, b"").unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    let (stdout, result) = files.write_with_stdout(0, b"lost");
    assert!(!stdout);
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn zero_mcd_cannot_be_closed() {
    let mut files = FileIo::default();
    assert_eq!(
        files.close(0).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    // An invalid close must not close stdout or an unrelated valid file.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("still-open.txt");
    let channel = files.open(&path, None);
    assert!(files.close(0).is_err());
    assert!(files.write(channel | 1, b"open").unwrap());
    files.close(channel).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"open");
}

#[test]
fn memories_preserve_four_state_and_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory");
    let value = BigUint::from(0xa5u8);
    let mask = BigUint::from(0xf0u8);
    let word = || DisplayFormatArg {
        value: &value,
        mask: Some(&mask),
        width: 8,
        signed: false,
        is_string: false,
        float_precision: None,
    };
    FileIo::write_memory(&path, true, [word()]).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "xzxz0101\n");
    FileIo::write_memory(&path, false, [word()]).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "X5\n");
}
