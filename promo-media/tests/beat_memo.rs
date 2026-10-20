//! The hash memo (docs/BEAT-SYNC.md §3.5): finding a song's cache hashes
//! its bytes once per (path, size, modification time) in a process. Its own
//! test binary, so no other test's hashing moves the count.

mod support;

use promo_media::beats;
use support::click_track::{click_track, wav};

#[test]
fn a_song_is_hashed_once_until_it_changes() {
    let dir = std::env::temp_dir().join(format!("promo-beat-memo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let song = dir.join("song.wav");
    let (samples, _) = click_track(120.0, 2.0, 44_100.0, None);
    std::fs::write(&song, wav(&samples, 1, 44_100)).unwrap();
    let before = beats::files_hashed();
    let first = beats::source_of(&song).unwrap();
    for _ in 0..3 {
        assert_eq!(beats::source_of(&song).unwrap(), first);
    }
    assert_eq!(
        beats::files_hashed(),
        before + 1,
        "hashed again for an unchanged file"
    );
    // Other bytes, another size: hashed again, another key.
    let (samples, _) = click_track(120.0, 3.0, 44_100.0, None);
    std::fs::write(&song, wav(&samples, 1, 44_100)).unwrap();
    let second = beats::source_of(&song).unwrap();
    assert_eq!(beats::files_hashed(), before + 2);
    assert_ne!(second.sha256, first.sha256);
    let _ = std::fs::remove_dir_all(&dir);
}
