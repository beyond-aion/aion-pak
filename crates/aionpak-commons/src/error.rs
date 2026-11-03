use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error(transparent)]
	Io(#[from] io::Error),
	#[error("{0}")]
	Format(String),
	#[error("{0}")]
	Xml(String),
	/// The file is not a zip archive at all (other programs ship `.pak` files of their own).
	#[error("not a zip archive")]
	NotArchive,
	/// The caller asked to stop.
	#[error("cancelled")]
	Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn format_err<T>(msg: impl Into<String>) -> Result<T> {
	Err(Error::Format(msg.into()))
}

pub(crate) fn xml_err<T>(msg: impl Into<String>) -> Result<T> {
	Err(Error::Xml(msg.into()))
}
