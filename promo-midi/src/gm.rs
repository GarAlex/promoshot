//! The General MIDI catalog: program names, the sixteen families, the
//! drum kits a GS bank offers on the percussion channel, and the drum key
//! map — what an editor's instrument picker and an agent's block both
//! spell instruments with.

/// GM Level 1 program names, 0-based.
pub const PROGRAMS: [&str; 128] = [
    "Acoustic Grand Piano",
    "Bright Acoustic Piano",
    "Electric Grand Piano",
    "Honky-tonk Piano",
    "Electric Piano 1",
    "Electric Piano 2",
    "Harpsichord",
    "Clavinet",
    "Celesta",
    "Glockenspiel",
    "Music Box",
    "Vibraphone",
    "Marimba",
    "Xylophone",
    "Tubular Bells",
    "Dulcimer",
    "Drawbar Organ",
    "Percussive Organ",
    "Rock Organ",
    "Church Organ",
    "Reed Organ",
    "Accordion",
    "Harmonica",
    "Tango Accordion",
    "Acoustic Guitar (nylon)",
    "Acoustic Guitar (steel)",
    "Electric Guitar (jazz)",
    "Electric Guitar (clean)",
    "Electric Guitar (muted)",
    "Overdriven Guitar",
    "Distortion Guitar",
    "Guitar Harmonics",
    "Acoustic Bass",
    "Electric Bass (finger)",
    "Electric Bass (pick)",
    "Fretless Bass",
    "Slap Bass 1",
    "Slap Bass 2",
    "Synth Bass 1",
    "Synth Bass 2",
    "Violin",
    "Viola",
    "Cello",
    "Contrabass",
    "Tremolo Strings",
    "Pizzicato Strings",
    "Orchestral Harp",
    "Timpani",
    "String Ensemble 1",
    "String Ensemble 2",
    "Synth Strings 1",
    "Synth Strings 2",
    "Choir Aahs",
    "Voice Oohs",
    "Synth Voice",
    "Orchestra Hit",
    "Trumpet",
    "Trombone",
    "Tuba",
    "Muted Trumpet",
    "French Horn",
    "Brass Section",
    "Synth Brass 1",
    "Synth Brass 2",
    "Soprano Sax",
    "Alto Sax",
    "Tenor Sax",
    "Baritone Sax",
    "Oboe",
    "English Horn",
    "Bassoon",
    "Clarinet",
    "Piccolo",
    "Flute",
    "Recorder",
    "Pan Flute",
    "Blown Bottle",
    "Shakuhachi",
    "Whistle",
    "Ocarina",
    "Lead 1 (square)",
    "Lead 2 (sawtooth)",
    "Lead 3 (calliope)",
    "Lead 4 (chiff)",
    "Lead 5 (charang)",
    "Lead 6 (voice)",
    "Lead 7 (fifths)",
    "Lead 8 (bass + lead)",
    "Pad 1 (new age)",
    "Pad 2 (warm)",
    "Pad 3 (polysynth)",
    "Pad 4 (choir)",
    "Pad 5 (bowed)",
    "Pad 6 (metallic)",
    "Pad 7 (halo)",
    "Pad 8 (sweep)",
    "FX 1 (rain)",
    "FX 2 (soundtrack)",
    "FX 3 (crystal)",
    "FX 4 (atmosphere)",
    "FX 5 (brightness)",
    "FX 6 (goblins)",
    "FX 7 (echoes)",
    "FX 8 (sci-fi)",
    "Sitar",
    "Banjo",
    "Shamisen",
    "Koto",
    "Kalimba",
    "Bagpipe",
    "Fiddle",
    "Shanai",
    "Tinkle Bell",
    "Agogo",
    "Steel Drums",
    "Woodblock",
    "Taiko Drum",
    "Melodic Tom",
    "Synth Drum",
    "Reverse Cymbal",
    "Guitar Fret Noise",
    "Breath Noise",
    "Seashore",
    "Bird Tweet",
    "Telephone Ring",
    "Helicopter",
    "Applause",
    "Gunshot",
];

/// The sixteen GM families, each eight programs from its first.
pub const FAMILIES: [&str; 16] = [
    "Piano",
    "Chromatic Percussion",
    "Organ",
    "Guitar",
    "Bass",
    "Strings",
    "Ensemble",
    "Brass",
    "Reed",
    "Pipe",
    "Synth Lead",
    "Synth Pad",
    "Synth Effects",
    "Ethnic",
    "Percussive",
    "Sound Effects",
];

/// The drum kits a GS bank keeps on the percussion channel, by program.
pub const DRUM_KITS: [(u8, &str); 9] = [
    (0, "Standard"),
    (8, "Room"),
    (16, "Power"),
    (24, "Electronic"),
    (25, "TR-808"),
    (32, "Jazz"),
    (40, "Brush"),
    (48, "Orchestra"),
    (56, "SFX"),
];

/// The GM percussion key map, keys 35–81.
pub const DRUM_KEYS: [(u8, &str); 47] = [
    (35, "Acoustic Bass Drum"),
    (36, "Bass Drum 1"),
    (37, "Side Stick"),
    (38, "Acoustic Snare"),
    (39, "Hand Clap"),
    (40, "Electric Snare"),
    (41, "Low Floor Tom"),
    (42, "Closed Hi-Hat"),
    (43, "High Floor Tom"),
    (44, "Pedal Hi-Hat"),
    (45, "Low Tom"),
    (46, "Open Hi-Hat"),
    (47, "Low-Mid Tom"),
    (48, "Hi-Mid Tom"),
    (49, "Crash Cymbal 1"),
    (50, "High Tom"),
    (51, "Ride Cymbal 1"),
    (52, "Chinese Cymbal"),
    (53, "Ride Bell"),
    (54, "Tambourine"),
    (55, "Splash Cymbal"),
    (56, "Cowbell"),
    (57, "Crash Cymbal 2"),
    (58, "Vibraslap"),
    (59, "Ride Cymbal 2"),
    (60, "Hi Bongo"),
    (61, "Low Bongo"),
    (62, "Mute Hi Conga"),
    (63, "Open Hi Conga"),
    (64, "Low Conga"),
    (65, "High Timbale"),
    (66, "Low Timbale"),
    (67, "High Agogo"),
    (68, "Low Agogo"),
    (69, "Cabasa"),
    (70, "Maracas"),
    (71, "Short Whistle"),
    (72, "Long Whistle"),
    (73, "Short Guiro"),
    (74, "Long Guiro"),
    (75, "Claves"),
    (76, "Hi Wood Block"),
    (77, "Low Wood Block"),
    (78, "Mute Cuica"),
    (79, "Open Cuica"),
    (80, "Mute Triangle"),
    (81, "Open Triangle"),
];

/// A program's GM name.
pub fn program_name(program: u8) -> &'static str {
    PROGRAMS[usize::from(program.min(127))]
}

/// A program's family.
pub fn family(program: u8) -> &'static str {
    FAMILIES[usize::from(program.min(127) / 8)]
}

/// The kit a percussion program selects: its own name, else the nearest
/// kit below it (a GS bank falls back the same way).
pub fn kit_name(program: u8) -> &'static str {
    DRUM_KITS
        .iter()
        .rev()
        .find(|(p, _)| *p <= program)
        .map(|(_, name)| *name)
        .unwrap_or("Standard")
}

/// A percussion key's name, for keys in the GM map.
pub fn drum_name(key: u8) -> Option<&'static str> {
    DRUM_KEYS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, name)| *name)
}

/// The catalog as JSON: `{families: [{name, first, programs: [{program,
/// name}]}], drumKits: [{program, name}], drumKeys: [{note, name}],
/// effects: [{preset, title, program, length}]}` — the instrument picker's
/// and an agent's one source.
pub fn catalog_json() -> serde_json::Value {
    use serde_json::json;
    let families: Vec<serde_json::Value> = FAMILIES
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let first = i * 8;
            json!({
                "name": name,
                "first": first,
                "programs": (first..first + 8)
                    .map(|p| json!({ "program": p, "name": PROGRAMS[p] }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "families": families,
        "drumKits": DRUM_KITS.iter().map(|(p, n)| json!({ "program": p, "name": n })).collect::<Vec<_>>(),
        "drumKeys": DRUM_KEYS.iter().map(|(k, n)| json!({ "note": k, "name": n })).collect::<Vec<_>>(),
        "effects": crate::effects::PRESETS.iter().map(|p| json!({
            "preset": p.id, "title": p.title, "program": p.program, "length": p.length,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_families() {
        assert_eq!(program_name(0), "Acoustic Grand Piano");
        assert_eq!(program_name(48), "String Ensemble 1");
        assert_eq!(program_name(80), "Lead 1 (square)");
        assert_eq!(program_name(88), "Pad 1 (new age)");
        assert_eq!(program_name(127), "Gunshot");
        assert_eq!(family(33), "Bass");
        assert_eq!(family(88), "Synth Pad");
        assert_eq!(kit_name(26), "TR-808");
        assert_eq!(drum_name(42), Some("Closed Hi-Hat"));
        assert_eq!(drum_name(20), None);
        let catalog = catalog_json();
        assert_eq!(catalog["families"].as_array().unwrap().len(), 16);
        assert_eq!(catalog["drumKeys"].as_array().unwrap().len(), 47);
    }
}
