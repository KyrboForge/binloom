use super::LinuxAbi;
use std::io::{Read, Seek, SeekFrom};

#[cfg(target_os = "linux")]
pub(super) fn detect_libc() -> Option<LinuxAbi> {
    // System executables provide evidence even when Binloom itself is static.
    // Inspect all available interpreters so mixed evidence fails closed.
    agreed_libc(
        ["/proc/self/exe", "/bin/sh", "/usr/bin/env"]
            .into_iter()
            .filter_map(|path| interpreter(&mut std::fs::File::open(path).ok()?)),
    )
}

fn agreed_libc(interpreters: impl IntoIterator<Item = impl AsRef<[u8]>>) -> Option<LinuxAbi> {
    fn classify(path: impl AsRef<[u8]>) -> Option<LinuxAbi> {
        match path.as_ref().rsplit(|byte| *byte == b'/').next()? {
            b"ld-linux-x86-64.so.2" | b"ld-linux-aarch64.so.1" => Some(LinuxAbi::Gnu),
            b"ld-musl-x86_64.so.1" | b"ld-musl-aarch64.so.1" => Some(LinuxAbi::Musl),
            _ => None,
        }
    }

    let mut interpreters = interpreters.into_iter().map(classify);
    let first = interpreters.next()??;
    interpreters.all(|abi| abi == Some(first)).then_some(first)
}

/// Read PT_INTERP from ELF64, the executable class of the supported Linux CPUs.
/// Read only the headers and interpreter path so even large executables are cheap.
fn interpreter(executable: &mut (impl Read + Seek)) -> Option<Vec<u8>> {
    const PROGRAM_HEADER_SIZE: u64 = 56;
    const PT_INTERP: u64 = 3;
    let mut header = [0; 64];
    executable.read_exact(&mut header).ok()?;
    if header[..5] != *b"\x7fELF\x02" || header[6] != 1 {
        return None;
    }

    let little_endian = match header[5] {
        1 => true,
        2 => false,
        _ => return None,
    };
    let number = |bytes: &[u8]| {
        let mut field = [0; 8];
        if little_endian {
            field[..bytes.len()].copy_from_slice(bytes);
            u64::from_le_bytes(field)
        } else {
            field[8 - bytes.len()..].copy_from_slice(bytes);
            u64::from_be_bytes(field)
        }
    };

    let offset = number(&header[32..40]);
    let count = number(&header[56..58]);
    let table_size = count * PROGRAM_HEADER_SIZE;
    let file_size = executable.seek(SeekFrom::End(0)).ok()?;
    if number(&header[54..56]) != PROGRAM_HEADER_SIZE
        || offset < 64
        || table_size > 65536
        || offset.checked_add(table_size)? > file_size
    {
        return None;
    }

    executable.seek(SeekFrom::Start(offset)).ok()?;
    let mut program = [0; PROGRAM_HEADER_SIZE as usize];
    let mut location = None;
    for _ in 0..count {
        executable.read_exact(&mut program).ok()?;
        if number(&program[..4]) == PT_INTERP {
            if location.is_some() {
                return None;
            }
            location = Some((number(&program[8..16]), number(&program[32..40])));
        }
    }
    let (offset, size) = location?;
    if !(2..=4096).contains(&size) || offset.checked_add(size)? > file_size {
        return None;
    }
    let mut path = vec![0; size as usize];
    executable.seek(SeekFrom::Start(offset)).ok()?;
    executable.read_exact(&mut path).ok()?;
    if path.pop()? != 0 || !path.starts_with(b"/") || path.contains(&0) {
        return None;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn elf(interpreter: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; 120];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[32..40].copy_from_slice(&64_u64.to_le_bytes()); // e_phoff
        bytes[54..56].copy_from_slice(&56_u16.to_le_bytes()); // e_phentsize
        bytes[56..58].copy_from_slice(&1_u16.to_le_bytes()); // e_phnum
        if !interpreter.is_empty() {
            bytes[64..68].copy_from_slice(&3_u32.to_le_bytes()); // PT_INTERP
            bytes[72..80].copy_from_slice(&120_u64.to_le_bytes()); // p_offset
            bytes[96..104].copy_from_slice(&((interpreter.len() + 1) as u64).to_le_bytes());
            bytes.extend_from_slice(interpreter);
            bytes.push(0);
        }
        bytes
    }

    #[test]
    fn elf64_interpreter_fixtures() {
        let path = b"/lib64/ld-linux-x86-64.so.2";
        let valid = elf(path);
        assert_eq!(
            interpreter(&mut Cursor::new(&valid)).as_deref(),
            Some(path.as_slice())
        );
        for invalid in [
            b"#!/bin/sh\n".as_slice(),
            &valid[..20],
            &valid[..100],
            &valid[..valid.len() - 1],
            &elf(b""),
        ] {
            assert_eq!(interpreter(&mut Cursor::new(invalid)), None);
        }
        let mut invalid = valid;
        invalid[72..80].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(interpreter(&mut Cursor::new(invalid)), None);
    }

    #[test]
    fn simulated_loader_evidence_must_be_recognized_and_agree() {
        for (path, abi) in [
            ("/lib64/ld-linux-x86-64.so.2", LinuxAbi::Gnu), // Ubuntu x86_64
            ("/lib/ld-musl-x86_64.so.1", LinuxAbi::Musl),   // Alpine x86_64
            ("/lib/ld-linux-aarch64.so.1", LinuxAbi::Gnu),
            ("/lib/ld-musl-aarch64.so.1", LinuxAbi::Musl),
        ] {
            let evidence = interpreter(&mut Cursor::new(elf(path.as_bytes()))).unwrap();
            assert_eq!(agreed_libc([&evidence, &evidence]), Some(abi));
        }
        assert_eq!(
            agreed_libc(["/lib64/ld-linux-x86-64.so.2", "/lib/ld-musl-x86_64.so.1"]),
            None
        );
        assert_eq!(
            agreed_libc(["/lib64/ld-linux-x86-64.so.2", "/loader/custom"]),
            None
        );
        assert_eq!(agreed_libc([] as [&str; 0]), None);
    }
}
