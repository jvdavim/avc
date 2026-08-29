//! Transfer progress, in the two forms a transfer gets watched.
//!
//! Somebody sitting at a terminal wants one line that stays put and moves. It
//! answers "is this stuck?" without scrolling the artifact lines away, and it
//! costs nothing once the command is done, because it erases itself.
//!
//! A pipeline wants the opposite. Its log is a file read after the fact, a
//! carriage return is just a character in it, and an animated line arrives as
//! thousands of unreadable fragments. So a build agent gets the same numbers as
//! ordinary lines on stdout, emitted rarely — often enough that a twenty-minute
//! upload never looks hung, seldom enough that the artifact lines are still
//! findable underneath.
//!
//! Which form is used is decided once, at startup, from `--progress` and the
//! environment; see [`resolve`]. Neither form is ever the only place a fact
//! appears — everything here is also in the summary the command prints when it
//! finishes — so a run whose progress was suppressed loses nothing at all.

use std::cell::RefCell;
use std::io::{IsTerminal, Read, Write};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use crate::ui::{self, Style};

/// How often the bar is redrawn. Fast enough to read as motion, slow enough
/// that a transfer off a local disk does not spend its time formatting bytes.
const BAR_INTERVAL: Duration = Duration::from_millis(80);

/// How often a pipeline gets a progress line. A log is read after the fact, so
/// what is wanted there is proof of life, not smoothness.
const LOG_INTERVAL: Duration = Duration::from_secs(10);

/// Characters between the brackets, at a terminal wide enough for them. A
/// narrow one gets a shorter bar rather than losing the numbers beside it,
/// which are the part that can be read out loud.
const BAR_WIDTH: usize = 24;
const MIN_BAR_WIDTH: usize = 8;

/// Assumed terminal width; see [`terminal_width`].
const DEFAULT_WIDTH: usize = 80;

/// Bounds on the room given to the path being transferred. Below the first
/// there is nothing worth reading left, so the label is dropped rather than
/// shown as an ellipsis and three characters; past the second, one long path
/// would crowd the rate and the estimate off even a wide terminal.
const MIN_LABEL: usize = 12;
const LABEL_WIDTH: usize = 26;

/// The resolved form, decided by [`init`] and read by every [`Progress`].
static MODE: AtomicU8 = AtomicU8::new(Mode::Off as u8);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lower")]
pub enum ProgressChoice {
    /// A bar at a terminal, periodic lines in a pipeline.
    #[default]
    Auto,
    Always,
    Never,
}

/// How progress is shown, once the question has been settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Off = 0,
    /// One line, redrawn in place on stderr.
    Bar = 1,
    /// Occasional whole lines on stdout, for a log.
    Log = 2,
}

impl Mode {
    fn from_code(code: u8) -> Mode {
        match code {
            1 => Mode::Bar,
            2 => Mode::Log,
            _ => Mode::Off,
        }
    }

    fn current() -> Mode {
        Mode::from_code(MODE.load(Ordering::Relaxed))
    }
}

/// Settle how progress is reported. Call once, before any transfer.
pub fn init(choice: ProgressChoice) {
    let mode = resolve(choice, std::io::stderr().is_terminal());
    MODE.store(mode as u8, Ordering::Relaxed);
}

/// The bar is for a workstation and nowhere else.
///
/// A build agent is checked for first, and beats the terminal test, because
/// some of them do allocate a pseudo-terminal: a runner that renders ANSI in
/// its log viewer would otherwise be handed an animation to store, which is
/// what makes CI logs full of `\r` and half-drawn bars. Everything that is not
/// a terminal — a pipe, a redirect, `TERM=dumb` — falls back to the same lines
/// the pipeline gets, since those survive being written to a file.
fn resolve(choice: ProgressChoice, is_terminal: bool) -> Mode {
    match choice {
        ProgressChoice::Never => Mode::Off,
        ProgressChoice::Always => Mode::Bar,
        ProgressChoice::Auto if in_ci() => Mode::Log,
        ProgressChoice::Auto if is_terminal && !dumb_terminal() => Mode::Bar,
        ProgressChoice::Auto => Mode::Log,
    }
}

/// Variables a build agent sets and a workstation does not.
///
/// `CI` alone covers GitHub Actions, GitLab CI, CircleCI, Travis, Buildkite,
/// Drone and Woodpecker; the rest are the well-known agents that do not set it.
const CI_MARKERS: [&str; 7] = [
    "CI",
    "CONTINUOUS_INTEGRATION",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "JENKINS_URL",
    "TEAMCITY_VERSION",
    // Azure Pipelines.
    "TF_BUILD",
];

fn in_ci() -> bool {
    CI_MARKERS.iter().any(|name| env_flag(name))
}

/// Whether `name` is set to something that means yes.
///
/// `CI=false` is a real thing a workflow writes to turn this off, so it counts
/// as unset rather than as the string "false" being present.
fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no"
        )
    })
}

fn dumb_terminal() -> bool {
    std::env::var("TERM").is_ok_and(|term| term == "dumb")
}

/// A transfer being watched.
///
/// Counts are kept behind a [`RefCell`] so a [`Meter`] can borrow the whole
/// thing while the bytes it measures are still streaming; a command is a single
/// thread, so there is nothing to lock.
pub struct Progress {
    mode: Mode,
    /// Present participle — `uploading`, `downloading` — which is what keeps a
    /// progress line from being mistaken for one of the `uploaded` lines that
    /// record what actually happened.
    verb: &'static str,
    state: RefCell<State>,
}

struct State {
    /// Objects the run expects to move, and bytes they add up to. Both can
    /// grow: a directory's manifest has to arrive before the files it names are
    /// known, so a pull that starts with one object may find it has thirty.
    objects: usize,
    total: u64,
    done_objects: usize,
    done: u64,
    /// What is moving now, shown so a stall names the file it stalled on.
    label: String,
    started: Instant,
    /// When something was last drawn or logged, so the cadence is honoured.
    last: Option<Instant>,
    /// Whether the terminal line currently holds a bar that has to be erased
    /// before anything else is printed.
    drawn: bool,
}

impl Progress {
    /// Report nothing at all: for `--porcelain`, whose stdout is a contract
    /// that a progress line would corrupt.
    pub fn off() -> Progress {
        Progress::new(Mode::Off, "", 0, 0)
    }

    /// Begin watching a transfer of `objects` objects totalling `bytes`.
    ///
    /// A run with nothing to move reports nothing: a push that finds the remote
    /// already holds every object should print its `up-to-date` lines and stop,
    /// not flash a bar that was full before it was drawn.
    pub fn start(verb: &'static str, objects: usize, bytes: u64) -> Progress {
        let mode = if objects == 0 {
            Mode::Off
        } else {
            Mode::current()
        };
        Progress::new(mode, verb, objects, bytes)
    }

    fn new(mode: Mode, verb: &'static str, objects: usize, total: u64) -> Progress {
        let started = Instant::now();
        Progress {
            mode,
            verb,
            state: RefCell::new(State {
                objects,
                total,
                done_objects: 0,
                done: 0,
                label: String::new(),
                started,
                // A pipeline's first line is due one interval in, so a transfer
                // that finishes quickly stays silent. A terminal draws at once.
                last: (mode == Mode::Log).then_some(started),
                drawn: false,
            }),
        }
    }

    /// Enlarge the total, for work that could not be counted until now.
    pub fn add(&self, objects: usize, bytes: u64) {
        let mut state = self.state.borrow_mut();
        state.objects += objects;
        state.total += bytes;
    }

    /// Name what is moving now.
    pub fn item(&self, label: &str) {
        self.state.borrow_mut().label = label.to_owned();
        self.tick();
    }

    /// Record `bytes` as moved.
    pub fn advance(&self, bytes: u64) {
        self.state.borrow_mut().done += bytes;
        self.tick();
    }

    /// Record one object as finished, however it was satisfied.
    pub fn object_done(&self) {
        self.state.borrow_mut().done_objects += 1;
    }

    /// Record one whole object as finished in a single step: work that was
    /// counted in the total but never streamed through a [`Meter`], such as a
    /// file already on disk or one copied out of a cache.
    pub fn done(&self, bytes: u64) {
        self.object_done();
        self.advance(bytes);
    }

    /// Wrap a reader so that everything read through it counts as moved.
    pub fn meter<'a>(&'a self, inner: &'a mut dyn Read) -> Meter<'a> {
        Meter {
            inner,
            progress: self,
        }
    }

    /// Take the terminal line back, so a normal line can be printed where the
    /// bar is. The bar reappears on the next tick, one line further down.
    pub fn clear(&self) {
        let mut state = self.state.borrow_mut();
        if !state.drawn {
            return;
        }
        state.drawn = false;
        // Redrawing immediately rather than at the next cadence point keeps the
        // bar from vanishing for a fraction of a second after every line.
        state.last = None;
        let mut stderr = std::io::stderr().lock();
        let _ = write!(stderr, "\r{}\r", " ".repeat(terminal_width()));
        let _ = stderr.flush();
    }

    /// Stop watching, leaving no trace: the command's own summary is the record
    /// of what happened.
    pub fn finish(&self) {
        self.clear();
    }

    /// Draw or log, if enough time has passed since the last time.
    fn tick(&self) {
        if self.mode == Mode::Off {
            return;
        }
        let mut state = self.state.borrow_mut();
        let interval = match self.mode {
            Mode::Log => LOG_INTERVAL,
            _ => BAR_INTERVAL,
        };
        let now = Instant::now();
        if let Some(last) = state.last {
            if now.duration_since(last) < interval {
                return;
            }
        }
        state.last = Some(now);
        match self.mode {
            Mode::Bar => {
                self.draw(&state);
                state.drawn = true;
            }
            Mode::Log => self.log(&state),
            Mode::Off => {}
        }
    }

    /// One line, redrawn over itself.
    ///
    /// Everything is measured before any escape code is added, and the line is
    /// padded to the full width, so the tail of a longer previous line is
    /// overwritten rather than left behind.
    fn draw(&self, state: &State) {
        let width = terminal_width();
        let mut line = format!(
            "[{}] {:>3}%",
            bar(
                state.fraction(),
                (width / 3).clamp(MIN_BAR_WIDTH, BAR_WIDTH)
            ),
            state.percent()
        );
        let mut used = self.verb.chars().count() + 1 + line.chars().count();

        // Appended in the order they earn their space, so a narrow terminal
        // loses the least useful part first: how far along, then how much,
        // then what is moving, and only then how fast and how much longer.
        //
        // Padded to a width that does not depend on the numbers in it, so the
        // segments to its right hold still rather than shuffling every time
        // `9.9 MiB` becomes `10.0 MiB`.
        let total = ui::size(state.total);
        append(
            &mut line,
            &mut used,
            width,
            &format!(
                "{:>size$}/{total}",
                ui::size(state.done),
                size = total.chars().count()
            ),
        );

        // Named before the rate, because a transfer that has stopped is only
        // diagnosable if the line says which file it stopped on. Capped, so one
        // long path cannot crowd everything else off a wide terminal.
        if !state.label.is_empty() {
            let room = width.saturating_sub(used + 2).min(LABEL_WIDTH);
            if room >= MIN_LABEL {
                append(&mut line, &mut used, width, &elide(&state.label, room));
            }
        }

        // Neither exists until enough has moved for it to mean anything, and an
        // estimate with no rate behind it would be a guess.
        if let Some(rate) = state.rate() {
            append(
                &mut line,
                &mut used,
                width,
                &format!("{}/s", ui::size(rate)),
            );
            if let Some(eta) = state.eta() {
                append(&mut line, &mut used, width, &format!("eta {}", clock(eta)));
            }
        }

        let padding = width.saturating_sub(used);
        let mut stderr = std::io::stderr().lock();
        let _ = write!(
            stderr,
            "\r{} {line}{}",
            ui::paint_err(self.verb, Style::Bold),
            " ".repeat(padding)
        );
        let _ = stderr.flush();
    }

    /// One whole line, in the vocabulary the rest of the output already uses.
    fn log(&self, state: &State) {
        let subject = format!(
            "{:>3}%  {}/{} objects  {}/{}",
            state.percent(),
            state.done_objects,
            state.objects,
            ui::size(state.done),
            ui::size(state.total)
        );
        let detail = state.rate().map(|rate| match state.eta() {
            Some(eta) => format!("{}/s, eta {}", ui::size(rate), clock(eta)),
            None => format!("{}/s", ui::size(rate)),
        });
        ui::action(self.verb, Style::Dim, &subject, detail.as_deref());
    }
}

/// A command that fails mid-transfer would otherwise print its error over a
/// half-drawn bar.
impl Drop for Progress {
    fn drop(&mut self) {
        self.clear();
    }
}

impl State {
    /// How far along, as a fraction.
    ///
    /// Bytes are the honest measure, except when there are none to count — a
    /// directory of empty files is still work — and then objects are.
    fn fraction(&self) -> f64 {
        let (done, total) = if self.total > 0 {
            (self.done as f64, self.total as f64)
        } else {
            (self.done_objects as f64, self.objects as f64)
        };
        if total <= 0.0 {
            return 0.0;
        }
        (done / total).clamp(0.0, 1.0)
    }

    /// The fraction as a whole number, held below 100 until there is genuinely
    /// nothing left.
    ///
    /// A total that is still growing would otherwise show 100% and then fall
    /// back: the first pull of a tracked directory knows only its manifest
    /// until that manifest arrives and names forty files. Reserving 100% for
    /// the end makes the number mean "done" rather than "done with what was
    /// known a moment ago".
    fn percent(&self) -> u64 {
        let percent = (self.fraction() * 100.0).round() as u64;
        if percent == 100 && self.done_objects < self.objects {
            return 99;
        }
        percent
    }

    /// Bytes per second over the whole run.
    ///
    /// An average rather than a recent window: it is the number that predicts
    /// how long the rest will take, and it does not lurch every time a small
    /// object lands between two large ones. Withheld until there is enough of a
    /// run to divide by, since the first tenth of a second implies anything.
    fn rate(&self) -> Option<u64> {
        let elapsed = self.started.elapsed().as_secs_f64();
        if self.done == 0 || elapsed < 0.5 {
            return None;
        }
        Some((self.done as f64 / elapsed) as u64)
    }

    fn eta(&self) -> Option<Duration> {
        let rate = self.rate()?;
        if rate == 0 || self.done >= self.total {
            return None;
        }
        Some(Duration::from_secs_f64(
            (self.total - self.done) as f64 / rate as f64,
        ))
    }
}

/// A reader that reports what passes through it.
pub struct Meter<'a> {
    inner: &'a mut dyn Read,
    progress: &'a Progress,
}

impl Read for Meter<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.progress.advance(read as u64);
        Ok(read)
    }
}

/// A transient line for a phase with nothing to measure — asking a remote what
/// it already holds, which is a wait with no numbers in it.
///
/// Erased when it goes out of scope, including on the way out of an error, so
/// it can never be the last thing left on the screen. A pipeline is told
/// nothing: the heading above already says what the command is doing, and a log
/// does not need to hear that a wait is in progress.
pub struct Status(bool);

impl Status {
    pub fn show(text: &str) -> Status {
        if Mode::current() != Mode::Bar {
            return Status(false);
        }
        let width = terminal_width();
        let mut stderr = std::io::stderr().lock();
        let _ = write!(
            stderr,
            "\r{}{}",
            elide(text, width),
            " ".repeat(width.saturating_sub(text.chars().count()))
        );
        let _ = stderr.flush();
        Status(true)
    }
}

impl Drop for Status {
    fn drop(&mut self) {
        if !self.0 {
            return;
        }
        let mut stderr = std::io::stderr().lock();
        let _ = write!(stderr, "\r{}\r", " ".repeat(terminal_width()));
        let _ = stderr.flush();
    }
}

/// Append `segment` to a bar under construction, if what is left of the line
/// has room for it and the two spaces that separate it from what is there.
fn append(line: &mut String, used: &mut usize, width: usize, segment: &str) {
    let needed = 2 + segment.chars().count();
    if *used + needed > width {
        return;
    }
    line.push_str("  ");
    line.push_str(segment);
    *used += needed;
}

/// The characters between the brackets, `=` behind a `>` at the front.
fn bar(fraction: f64, width: usize) -> String {
    let filled = (width as f64 * fraction).round() as usize;
    (0..width)
        .map(|index| {
            if index + 1 < filled || filled == width {
                '='
            } else if index < filled {
                '>'
            } else {
                ' '
            }
        })
        .collect()
}

/// Shorten `text` to `width`, keeping the end.
///
/// The tail of a path is the part that identifies it — `...bert/weights.bin`
/// says more than `models/very/long/…` does.
fn elide(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width {
        return text.to_owned();
    }
    if width <= 3 {
        return ".".repeat(width);
    }
    let tail: String = text.chars().skip(count - (width - 3)).collect();
    format!("...{tail}")
}

/// A duration as a clock reading: `4:07`, or `1:12:30` once there are hours.
fn clock(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// The width to lay a bar out in.
///
/// Taken from `COLUMNS` when a shell exports it, and otherwise assumed to be
/// the classic 80. Guessing narrow is safe and guessing wide is not: a line
/// longer than the terminal wraps, and a wrapped line cannot be erased with a
/// carriage return, so the bar would scroll down the screen leaving a trail of
/// itself. Measuring the real width means an ioctl, and that means a dependency
/// this crate does not otherwise need.
fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .filter(|width| *width >= 40)
        .unwrap_or(DEFAULT_WIDTH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pipeline_never_gets_an_animation() {
        // Even with a pseudo-terminal, which some runners do allocate.
        assert_eq!(resolve(ProgressChoice::Never, true), Mode::Off);
        assert_eq!(resolve(ProgressChoice::Always, false), Mode::Bar);
        // Auto reads process-wide environment variables, so only the half that
        // does not depend on them is safe to assert in a parallel test run.
        assert_ne!(resolve(ProgressChoice::Auto, false), Mode::Bar);
    }

    #[test]
    fn ci_markers_distinguish_set_from_switched_off() {
        // Uniquely named, so no other test in this process reads it.
        let name = "AVC_TEST_CI_MARKER_ONLY_READ_HERE";
        assert!(!env_flag(name));
        for value in ["true", "1", "https://jenkins.example/job/7"] {
            std::env::set_var(name, value);
            assert!(env_flag(name), "{value}");
        }
        // The ways a workflow says "not this one", which mean unset rather than
        // the string `false` being present.
        for value in ["", "0", "false", "FALSE", "no"] {
            std::env::set_var(name, value);
            assert!(!env_flag(name), "{value}");
        }
        std::env::remove_var(name);
    }

    #[test]
    fn the_bar_fills_from_empty_to_full() {
        assert_eq!(bar(0.0, BAR_WIDTH), " ".repeat(BAR_WIDTH));
        assert_eq!(bar(1.0, BAR_WIDTH), "=".repeat(BAR_WIDTH));
        assert_eq!(bar(0.25, BAR_WIDTH), "=====>                  ");
        // Every rendering is exactly the width it was asked for, or the line it
        // sits on cannot be erased by overwriting it.
        for step in 0..=100 {
            for width in [MIN_BAR_WIDTH, 13, BAR_WIDTH] {
                assert_eq!(bar(f64::from(step) / 100.0, width).chars().count(), width);
            }
        }
    }

    #[test]
    fn a_shortened_path_keeps_the_end_that_identifies_it() {
        assert_eq!(elide("models/bert", 20), "models/bert");
        assert_eq!(elide("models/bert/weights.bin", 14), "...weights.bin");
        // Counted in characters, not bytes, so a non-ASCII path still fits the
        // column it was measured for.
        assert_eq!(elide("modelos/modêlo.bin", 10).chars().count(), 10);
    }

    #[test]
    fn an_estimate_reads_as_a_clock() {
        assert_eq!(clock(Duration::from_secs(7)), "0:07");
        assert_eq!(clock(Duration::from_secs(247)), "4:07");
        assert_eq!(clock(Duration::from_secs(4350)), "1:12:30");
    }

    #[test]
    fn progress_is_measured_in_bytes_unless_there_are_none() {
        let progress = Progress::new(Mode::Off, "testing", 4, 400);
        progress.done(100);
        assert_eq!(progress.state.borrow().percent(), 25);
        // Beyond the total is still the total: a remote that sends more than it
        // promised is caught by verification, not by the bar. And the last
        // object is still outstanding, so this is not yet done.
        progress.advance(1_000);
        assert_eq!(progress.state.borrow().percent(), 99);

        for _ in 0..3 {
            progress.object_done();
        }
        assert_eq!(progress.state.borrow().percent(), 100);

        // Four empty files are four things to do, not zero.
        let empty = Progress::new(Mode::Off, "testing", 4, 0);
        empty.object_done();
        assert_eq!(empty.state.borrow().percent(), 25);
    }

    #[test]
    fn a_rate_and_an_estimate_wait_for_enough_of_a_run_to_divide_by() {
        let progress = Progress::new(Mode::Off, "testing", 1, 4_000);
        progress.advance(1_000);
        // A tenth of a second of transfer implies whatever you like.
        assert_eq!(progress.state.borrow().rate(), None);

        // Four seconds in, a quarter done: 250 bytes a second, twelve to go.
        // Asserted as ranges, since the clock keeps running underneath.
        progress.state.borrow_mut().started = Instant::now() - Duration::from_secs(4);
        assert!(matches!(progress.state.borrow().rate(), Some(240..=250)));
        assert!(matches!(
            progress.state.borrow().eta().map(|eta| eta.as_secs()),
            Some(12..=13)
        ));

        // Nothing is left, so there is nothing to wait for.
        progress.advance(3_000);
        assert_eq!(progress.state.borrow().eta(), None);
    }

    #[test]
    fn a_growing_total_moves_the_bar_backwards_not_past_the_end() {
        let progress = Progress::new(Mode::Off, "testing", 1, 100);
        progress.done(100);
        assert_eq!(progress.state.borrow().percent(), 100);
        // A directory's manifest lands and names files nobody had counted yet.
        progress.add(3, 300);
        assert_eq!(progress.state.borrow().percent(), 25);
        progress.done(100);
        assert_eq!(progress.state.borrow().percent(), 50);
    }
}
