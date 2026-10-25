//! `promo <command> --help` (issue #20): every command answers with its own
//! usage and options and exits 0 — `promo words --help` used to fail with
//! "unknown option" and the global usage, exit 1.

use std::process::Command;

fn promo(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .args(args)
        .output()
        .expect("run promo");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The commands the global usage lists: the word after `promo` on each
/// usage line (`media activity` as two).
fn commands() -> Vec<String> {
    let (ok, usage, _) = promo(&["--help"]);
    assert!(ok);
    let mut found: Vec<String> = Vec::new();
    for line in usage.lines() {
        let Some(rest) = line.strip_prefix("    promo ") else {
            continue;
        };
        let mut words = rest.split_whitespace();
        let Some(first) = words.next() else { continue };
        let command = if first == "media" {
            format!("media {}", words.next().unwrap_or_default())
        } else {
            first.to_string()
        };
        if !found.contains(&command) {
            found.push(command);
        }
    }
    found
}

/// Every command: exit 0, its own usage first, and an OPTIONS entry for
/// every flag its usage names — nothing left described only elsewhere.
#[test]
fn every_command_answers_help_with_its_own_options() {
    let commands = commands();
    for expected in [
        "validate",
        "words",
        "beats",
        "media activity",
        "video",
        "init",
        "diff",
    ] {
        assert!(
            commands.iter().any(|c| c == expected),
            "{expected} in {commands:?}"
        );
    }
    for command in &commands {
        let mut args: Vec<&str> = command.split(' ').collect();
        args.push("--help");
        let (ok, text, err) = promo(&args);
        assert!(ok, "promo {command} --help fails: {err}");
        assert!(
            text.starts_with(&format!("USAGE:\n    promo {command}")),
            "promo {command} --help: {text}"
        );
        assert!(!err.contains("unknown option"), "{command}: {err}");
        let usage = text.split("\nOPTIONS:\n").next().unwrap();
        let options = text.split("\nOPTIONS:\n").nth(1).unwrap_or_default();
        for flag in usage
            .split(|c: char| c.is_whitespace() || "[]|,(){}".contains(c))
            .filter(|w| w.starts_with("--") && w.len() > 2)
            .map(|w| w.trim_end_matches(['.', ';', ':']))
        {
            assert!(
                options.lines().any(|l| l
                    .trim_start()
                    .split(|c: char| c.is_whitespace() || c == '/' || c == ',')
                    .any(|w| w == flag)),
                "promo {command} --help names {flag} without its entry:\n{text}"
            );
        }
    }
}

/// The issue's command, `-h` too, and the other verbs with flags of their
/// own; a word that is no command still fails.
#[test]
fn words_beats_and_media_help_name_their_flags() {
    for args in [
        &["words", "--help"][..],
        &["words", "-h"],
        &["words", "take.mp3", "--help"],
    ] {
        let (ok, text, err) = promo(args);
        assert!(ok, "{args:?}: {err}");
        for flag in [
            "--import",
            "--provider",
            "--transcribe",
            "--prune",
            "--timeline",
        ] {
            assert!(
                text.contains(&format!("\n    {flag}")),
                "{args:?} lists {flag}: {text}"
            );
        }
        assert!(text.contains("remove the records"), "{text}");
        assert!(
            !text.contains("render a PromoShot project folder"),
            "not the global usage"
        );
    }
    let (ok, text, _) = promo(&["beats", "--help"]);
    assert!(
        ok && text.contains("\n    --bpm-hint") && text.contains("\n    --meter"),
        "{text}"
    );
    for args in [&["media", "--help"][..], &["media", "activity", "--help"]] {
        let (ok, text, _) = promo(args);
        assert!(ok && text.contains("\n    --masks"), "{args:?}: {text}");
    }
    let (ok, text, _) = promo(&["validate", "--help"]);
    assert!(
        ok && text.contains("--strict") && text.contains("`validate` decodes"),
        "{text}"
    );
    let (ok, _, err) = promo(&["nonsense", "--help"]);
    assert!(!ok && err.contains("unknown command `nonsense`"), "{err}");
}
