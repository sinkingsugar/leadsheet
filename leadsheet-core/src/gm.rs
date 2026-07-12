//! General MIDI lookup tables: program → name, and instrument-label → program
//! for MuScriptor streams that only give us a string.

/// GM program names, index = program 0..=127.
pub const PROGRAM_NAMES: [&str; 128] = [
    "acoustic_grand_piano",
    "bright_acoustic_piano",
    "electric_grand_piano",
    "honky_tonk_piano",
    "electric_piano_1",
    "electric_piano_2",
    "harpsichord",
    "clavinet",
    "celesta",
    "glockenspiel",
    "music_box",
    "vibraphone",
    "marimba",
    "xylophone",
    "tubular_bells",
    "dulcimer",
    "drawbar_organ",
    "percussive_organ",
    "rock_organ",
    "church_organ",
    "reed_organ",
    "accordion",
    "harmonica",
    "tango_accordion",
    "nylon_guitar",
    "steel_guitar",
    "jazz_guitar",
    "clean_guitar",
    "muted_guitar",
    "overdriven_guitar",
    "distortion_guitar",
    "guitar_harmonics",
    "acoustic_bass",
    "fingered_bass",
    "picked_bass",
    "fretless_bass",
    "slap_bass_1",
    "slap_bass_2",
    "synth_bass_1",
    "synth_bass_2",
    "violin",
    "viola",
    "cello",
    "contrabass",
    "tremolo_strings",
    "pizzicato_strings",
    "orchestral_harp",
    "timpani",
    "string_ensemble_1",
    "string_ensemble_2",
    "synth_strings_1",
    "synth_strings_2",
    "choir_aahs",
    "voice_oohs",
    "synth_voice",
    "orchestra_hit",
    "trumpet",
    "trombone",
    "tuba",
    "muted_trumpet",
    "french_horn",
    "brass_section",
    "synth_brass_1",
    "synth_brass_2",
    "soprano_sax",
    "alto_sax",
    "tenor_sax",
    "baritone_sax",
    "oboe",
    "english_horn",
    "bassoon",
    "clarinet",
    "piccolo",
    "flute",
    "recorder",
    "pan_flute",
    "blown_bottle",
    "shakuhachi",
    "whistle",
    "ocarina",
    "square_lead",
    "sawtooth_lead",
    "calliope_lead",
    "chiff_lead",
    "charang_lead",
    "voice_lead",
    "fifths_lead",
    "bass_and_lead",
    "new_age_pad",
    "warm_pad",
    "polysynth_pad",
    "choir_pad",
    "bowed_pad",
    "metallic_pad",
    "halo_pad",
    "sweep_pad",
    "rain_fx",
    "soundtrack_fx",
    "crystal_fx",
    "atmosphere_fx",
    "brightness_fx",
    "goblins_fx",
    "echoes_fx",
    "sci_fi_fx",
    "sitar",
    "banjo",
    "shamisen",
    "koto",
    "kalimba",
    "bagpipe",
    "fiddle",
    "shanai",
    "tinkle_bell",
    "agogo",
    "steel_drums",
    "woodblock",
    "taiko_drum",
    "melodic_tom",
    "synth_drum",
    "reverse_cymbal",
    "guitar_fret_noise",
    "breath_noise",
    "seashore",
    "bird_tweet",
    "telephone_ring",
    "helicopter",
    "applause",
    "gunshot",
];

/// Short label for a GM program, used to name tracks that carry no name.
pub fn program_name(program: u8) -> &'static str {
    PROGRAM_NAMES[(program as usize).min(127)]
}

/// Map a free-form instrument label (MuScriptor gives strings like "bass",
/// "acoustic_piano", "drums") to a GM program. Returns `None` for percussion
/// labels — the caller should route those to the drum channel instead.
pub fn program_for_label(label: &str) -> Option<u8> {
    let l = crate::model::sanitize_name(label);
    if l.contains("drum") || l.contains("perc") || l.contains("kit") {
        return None;
    }
    // Exact GM name first.
    if let Some(p) = PROGRAM_NAMES.iter().position(|n| *n == l) {
        return Some(p as u8);
    }
    // Common aliases, checked as substrings (order matters: specific first).
    const ALIASES: &[(&str, u8)] = &[
        ("grand_piano", 0),
        ("electric_piano", 4),
        ("piano", 0),
        ("rhodes", 4),
        ("keys", 0),
        ("organ", 16),
        ("accordion", 21),
        ("harmonica", 22),
        ("nylon", 24),
        ("acoustic_guitar", 25),
        ("clean", 27),
        ("overdrive", 29),
        ("distort", 30),
        ("guitar", 25),
        ("acoustic_bass", 32),
        ("upright", 32),
        ("synth_bass", 38),
        ("bass", 33),
        ("violin", 40),
        ("viola", 41),
        ("cello", 42),
        ("string", 48),
        ("harp", 46),
        ("timpani", 47),
        ("choir", 52),
        ("voice", 53),
        ("vocal", 52),
        ("trumpet", 56),
        ("trombone", 57),
        ("tuba", 58),
        ("horn", 60),
        ("brass", 61),
        ("sax", 65),
        ("oboe", 68),
        ("clarinet", 71),
        ("flute", 73),
        ("lead", 81),
        ("pad", 89),
        ("synth", 81),
        ("vibraphone", 11),
        ("marimba", 12),
        ("xylophone", 13),
        ("bell", 14),
        ("sitar", 104),
        ("banjo", 105),
    ];
    for (alias, program) in ALIASES {
        if l.contains(alias) {
            return Some(*program);
        }
    }
    Some(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(program_for_label("bass"), Some(33));
        assert_eq!(program_for_label("Acoustic Piano"), Some(0));
        assert_eq!(program_for_label("synth_bass"), Some(38));
        assert_eq!(program_for_label("drums"), None);
        assert_eq!(program_for_label("Percussion"), None);
        assert_eq!(program_for_label("weird_thing"), Some(0));
        assert_eq!(program_for_label("fingered_bass"), Some(33));
    }
}
