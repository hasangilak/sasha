//! The tower's words laid out under its sound, on one line for both, so
//! each word sits under the sound of it. The words read as text, a space
//! apart, with a wider gap where the voice pauses; the sound is stretched
//! or squeezed to fit above them.

use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

/// A word, and when it's said, in seconds from the start.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(from = "(String, f64, f64)", into = "(String, f64, f64)")]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

impl From<(String, f64, f64)> for Word {
    fn from((text, start, end): (String, f64, f64)) -> Self {
        Word { text, start, end }
    }
}

impl From<Word> for (String, f64, f64) {
    fn from(word: Word) -> Self {
        (word.text, word.start, word.end)
    }
}

/// Pauses shorter than this are just the space between two words.
const PAUSE_MIN: f64 = 0.2;
/// The columns each second of a longer pause opens, up to `PAUSE_MAX`.
const PAUSE_COLUMNS: f64 = 10.0;
const PAUSE_MAX: f64 = 8.0;

/// Where the words go, and which time each column stands for.
pub struct Timeline<'a> {
    /// Each word, with the column it starts at.
    pub words: Vec<(u32, &'a Word)>,
    /// (column, time) points, both rising, joined by straight lines.
    stops: Vec<(f64, f64)>,
}

impl<'a> Timeline<'a> {
    pub fn new(words: &'a [Word], duration: f64) -> Self {
        let mut placed = Vec::with_capacity(words.len());
        let mut stops = vec![(0.0, 0.0)];
        let (mut column, mut time) = (0.0, 0.0_f64);
        for (i, word) in words.iter().enumerate() {
            let start = word.start.max(time);
            let end = word.end.max(start);
            let space = if i == 0 { 0.0 } else { 1.0 };
            column += space + pause(start - time);
            stops.push((column, start));
            placed.push((column as u32, word));
            column += word.text.width() as f64;
            stops.push((column, end));
            time = end;
        }
        if duration > time {
            stops.push((column + pause(duration - time), duration));
        }
        Timeline { words: placed, stops }
    }

    /// How many columns it takes.
    pub fn width(&self) -> u32 {
        self.stops.last().map_or(0, |&(column, _)| column as u32)
    }

    /// The column `time` falls at.
    pub fn column(&self, time: f64) -> f64 {
        along(self.stops.iter().map(|&(column, time)| (time, column)), time)
    }

    /// The time at `column`.
    pub fn time(&self, column: f64) -> f64 {
        along(self.stops.iter().copied(), column)
    }
}

/// The extra columns a pause of `seconds` opens.
fn pause(seconds: f64) -> f64 {
    ((seconds - PAUSE_MIN) * PAUSE_COLUMNS).round().clamp(0.0, PAUSE_MAX)
}

/// The y at `x` on the line through `points`, which rise in x.
fn along(points: impl Iterator<Item = (f64, f64)>, x: f64) -> f64 {
    let mut last: Option<(f64, f64)> = None;
    for (px, py) in points {
        match last {
            None if x <= px => return py,
            Some((lx, ly)) if x <= px && px > lx => {
                return if x <= lx { ly } else { ly + (py - ly) * (x - lx) / (px - lx) };
            }
            _ => {}
        }
        last = Some((px, py));
    }
    last.map_or(0.0, |(_, y)| y)
}

/// The first column shown in a view `view` wide, so where it's playing
/// stays a third of the way in while there's more to come.
pub fn scroll(playing: f64, width: u32, view: u16) -> u32 {
    let view = u32::from(view);
    if width <= view {
        return 0;
    }
    ((playing - f64::from(view / 3)).max(0.0) as u32).min(width - view)
}

/// When each word of `text` is said, for a voice that didn't say: spread
/// over `duration` by their length.
pub fn spread(text: &str, duration: f64) -> Vec<Word> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let total: usize = words.iter().map(|word| word.chars().count() + 1).sum();
    let mut at = 0;
    let mut out = Vec::with_capacity(words.len());
    for word in words {
        let length = word.chars().count();
        let time = |chars: usize| duration * chars as f64 / total.max(1) as f64;
        out.push(Word { text: word.to_owned(), start: time(at), end: time(at + length) });
        at += length + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, start: f64, end: f64) -> Word {
        Word { text: text.to_owned(), start, end }
    }

    #[test]
    fn words_read_a_space_apart_and_a_pause_opens_a_gap() {
        let words = [word("Nothing", 0.0, 0.4), word("new.", 0.4, 0.7), word("Today", 1.5, 1.9)];
        let line = Timeline::new(&words, 2.0);
        let columns: Vec<u32> = line.words.iter().map(|&(column, _)| column).collect();
        // "Nothing new." then six more columns for the 0.8s pause.
        assert_eq!(columns, [0, 8, 19]);
        assert_eq!(line.width(), 24);
    }

    #[test]
    fn each_word_sits_under_the_sound_of_it() {
        let words = [word("Hello", 0.5, 1.0), word("there", 1.0, 1.5)];
        let line = Timeline::new(&words, 1.5);
        let (hello, there) = (line.words[0].0, line.words[1].0);
        assert_eq!(line.time(f64::from(hello)), 0.5);
        assert_eq!(line.time(f64::from(there)), 1.0);
        // Halfway through a word, halfway across it.
        assert_eq!(line.column(0.75), f64::from(hello) + 2.5);
        assert_eq!(line.column(1.25), f64::from(there) + 2.5);
        // The pause before the first word opens a little room; past the
        // end is at the end.
        assert_eq!(hello, 3);
        assert_eq!(line.column(9.0), f64::from(line.width()));
    }

    #[test]
    fn a_word_out_of_order_waits_for_the_one_before() {
        let words = [word("one", 0.0, 1.0), word("two", 0.5, 0.4)];
        let line = Timeline::new(&words, 1.0);
        assert_eq!(line.words[1].0, 4);
        assert_eq!(line.time(4.0), 1.0);
        assert_eq!(line.time(7.0), 1.0);
    }

    #[test]
    fn it_runs_past_with_the_playing_a_third_of_the_way_in() {
        assert_eq!(scroll(50.0, 40, 60), 0);
        assert_eq!(scroll(10.0, 200, 60), 0);
        assert_eq!(scroll(50.0, 200, 60), 30);
        assert_eq!(scroll(195.0, 200, 60), 140);
    }

    #[test]
    fn words_without_timing_share_the_time_by_length() {
        let words = spread("ab cd", 6.0);
        assert_eq!(words, [word("ab", 0.0, 2.0), word("cd", 3.0, 5.0)]);
        assert!(spread("", 3.0).is_empty());
    }
}
