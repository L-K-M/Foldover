use std::io;

const CODE_HASH_HEX_LENGTH: usize = 40;

pub(super) struct CodeIdentity {
    pub identifier: String,
    pub hash: String,
    pub hardened: bool,
}

impl CodeIdentity {
    pub fn parse(output: &str) -> io::Result<Self> {
        let identifier = field(output, "Identifier=")?;
        let hash = field(output, "CDHash=")?;
        if hash.len() != CODE_HASH_HEX_LENGTH || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(invalid());
        }
        let header = field(output, "CodeDirectory ")?;
        let flags = header.split_once(" flags=").ok_or_else(invalid)?.1;
        let flags = flags.split_whitespace().next().ok_or_else(invalid)?;
        let names = flags.split_once('(').ok_or_else(invalid)?.1;
        let names = names.strip_suffix(')').ok_or_else(invalid)?;
        Ok(Self {
            identifier: identifier.to_owned(),
            hash: hash.to_ascii_lowercase(),
            hardened: names.split(',').any(|name| name == "runtime"),
        })
    }
}

fn field<'a>(output: &'a str, prefix: &str) -> io::Result<&'a str> {
    let mut values = output.lines().filter_map(|line| line.strip_prefix(prefix));
    let value = values
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(invalid)?;
    if values.next().is_some() {
        return Err(invalid());
    }
    Ok(value)
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Ambiguous or unsupported code-signing information",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

    fn information(flags: &str) -> String {
        format!(
            "Identifier=ch.lkmc.foldover.cli\nCodeDirectory v=20500 size=900 flags={flags} hashes=2+7\nCDHash={HASH}\n"
        )
    }

    #[test]
    fn reads_exact_identity_and_hardened_runtime() {
        let code = CodeIdentity::parse(&information("0x10002(adhoc,runtime)")).unwrap();
        assert_eq!(code.identifier, "ch.lkmc.foldover.cli");
        assert_eq!(code.hash, HASH);
        assert!(code.hardened);
        assert!(
            !CodeIdentity::parse(&information("0x2(adhoc)"))
                .unwrap()
                .hardened
        );
    }

    #[test]
    fn rejects_duplicate_injected_missing_and_malformed_fields() {
        let valid = information("0x10002(adhoc,runtime)");
        for invalid in [
            format!("{valid}CDHash={HASH}\n"),
            format!("Identifier=injected\n{valid}"),
            valid.replace(HASH, "not-a-hash"),
            valid.replace("Identifier=ch.lkmc.foldover.cli\n", ""),
            valid.replace("flags=0x10002(adhoc,runtime)", "flags=unknown"),
        ] {
            assert!(CodeIdentity::parse(&invalid).is_err(), "{invalid}");
        }
    }
}
