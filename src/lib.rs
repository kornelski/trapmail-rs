use failure::Fail;
use lazy_static::lazy_static;
use nix::unistd::Pid;
use regex::Regex;
use serde_derive::{Deserialize, Serialize};
use std::{env, fs, io, path, thread, time};
use structopt::StructOpt;

pub mod serde_pid;

/// Name of the environment variable indicating where to store mail.
pub const ENV_MAIL_STORE_PATH: &'static str = "TRAPMAIL_STORE";

/// Path to use in absence of `ENV_MAIL_STORE_PATH`.
const DEFAULT_MAIL_STORE_PATH: &'static str = "/tmp";

lazy_static! {
    /// Regular expression that matches filenames generated by `Mail`.
    static ref FILENAME_RE: Regex = Regex::new(r"trapmail_\d+_\d+_\d+.json").unwrap();
}

/// Command-line options for the `trapmail` program.
#[derive(Clone, Debug, Deserialize, Serialize, StructOpt)]
pub struct CliOptions {
    /// Non-standard debug output
    #[structopt(long = "debug")]
    pub debug: bool,
    /// Ignore dots alone on lines by themselves in incoming message
    #[structopt(short = "i")]
    pub ignore_dots: bool,
    /// Read message for recipient list
    #[structopt(short = "t")]
    pub inline_recipients: bool,
    /// Addresses to send mail to
    pub addresses: Vec<String>,
    /// Ignore everything else and dump the contents of an email file instead.
    #[structopt(long = "dump")]
    pub dump: Option<path::PathBuf>,
}

#[derive(Debug, Fail)]
pub enum Error {
    /// Failure to store email in store.
    #[fail(display = "Could not store mail: {}", 0)]
    Store(io::Error),
    /// Failure to serialize email to store.
    #[fail(display = "Could not serialize mail: {}", 0)]
    MailSerialization(serde_json::Error),
    /// Failure to enumerate files in directory
    #[fail(display = "Could not open storage directory for reading: {}", 0)]
    DirEnumeration(io::Error),
    /// Failure to load email from store.
    #[fail(display = "Could not load mail: {}", 0)]
    Load(io::Error),
    /// Failure to deserialize email from store.
    #[fail(display = "Could not deserialize mail: {}", 0)]
    MailDeserialization(serde_json::Error),
}

type Result<T> = ::std::result::Result<T, Error>;

/// A "sent" mail.
#[derive(Debug, Deserialize, Serialize)]
pub struct Mail {
    /// The command line arguments passed to `trapmail` at the time of call.
    pub cli_options: CliOptions,
    /// The ID of the `trapmail` process that stored this email.
    #[serde(with = "serde_pid")]
    pub pid: Pid,
    /// The ID of the parent process that called `trapmail`.
    #[serde(with = "serde_pid")]
    pub ppid: Pid,
    /// The `trapmail` call's raw body.
    #[serde(with = "serde_bytes")]
    pub raw_body: Vec<u8>,
    /// A microsecond-resolution UNIX timestamp of when the mail arrived.
    pub timestamp_us: u128,
}

impl Mail {
    /// Create a new `Mail` using the current time and process information.
    ///
    /// This function will sleep for a microsecond to avoid any conflicts in
    /// naming (see `file_name`).
    ///
    /// # Panics
    ///
    /// Will panic if the system returns a time before the UNIX epoch.
    pub fn new(cli_options: CliOptions, raw_body: Vec<u8>) -> Self {
        // We always sleep a microsecond, which is probably overkill, but
        // guarantees no collisions, ever (a millions mails a second ought
        // to be enough for even future test cases).
        thread::sleep(time::Duration::from_nanos(1000));

        let timestamp_us = (time::SystemTime::now().duration_since(time::UNIX_EPOCH))
            .expect("Got current before 1970; is your clock broken?")
            .as_micros();

        Mail {
            cli_options,
            raw_body,
            pid: nix::unistd::Pid::this(),
            ppid: nix::unistd::Pid::parent(),
            timestamp_us,
        }
    }

    /// Create a (pathless) file_name depending on the `Mail` contents.
    pub fn file_name(&self) -> path::PathBuf {
        format!(
            "trapmail_{}_{}_{}.json",
            self.timestamp_us, self.ppid, self.pid,
        )
        .into()
    }

    /// Load a `Mail` from a file.
    pub fn load<P: AsRef<path::Path>>(source: P) -> Result<Self> {
        serde_json::from_reader(fs::File::open(source).map_err(Error::Load)?)
            .map_err(Error::MailDeserialization)
    }
}

/// Mail storage.
#[derive(Debug)]
pub struct MailStore {
    /// Root path where all mail in this store gets stored.
    root: path::PathBuf,
}

impl MailStore {
    /// Construct new `MailStore` with path from environment.
    pub fn new() -> Self {
        Self::with_root(
            env::var(ENV_MAIL_STORE_PATH)
                .unwrap_or(DEFAULT_MAIL_STORE_PATH.to_owned())
                .into(),
        )
    }

    /// Construct new `MailStore` with explicit path.
    pub fn with_root(root: path::PathBuf) -> Self {
        MailStore { root }
    }

    /// Add a mail to the `MailStore`.
    ///
    /// Returns the path where the mail has been stored.
    pub fn add(&self, mail: &Mail) -> Result<path::PathBuf> {
        let output_fn = self.root.join(mail.file_name());

        serde_json::to_writer_pretty(fs::File::create(&output_fn).map_err(Error::Store)?, mail)
            .map_err(Error::MailSerialization)?;
        Ok(output_fn)
    }

    /// Iterate over all mails in storage.
    ///
    /// Mails are ordered by timestamp.
    pub fn iter_mails(&self) -> Result<impl Iterator<Item = Result<Mail>>> {
        // Use non-functional style here, as the nested `Result`s otherwise get
        // a bit hairy.
        let mut paths = Vec::new();

        // We read the contents of the entire directory first for sorting.
        for dir_result in fs::read_dir(&self.root).map_err(Error::DirEnumeration)? {
            let dir_entry = dir_result.map_err(Error::DirEnumeration)?;
            let filename = dir_entry
                .file_name()
                .into_string()
                .expect("OsString to String conversion should not fail for prefiltered filename.");

            if FILENAME_RE.is_match(&filename) {
                paths.push(filename);
            }
        }

        // All files are named `trapmail_TIMESTAMP_..` and thus will be sorted
        // correctly, even when sorted by filename.
        paths.sort();

        Ok(paths.into_iter().map(Mail::load))
    }
}