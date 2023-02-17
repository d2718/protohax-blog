/*!
 For easier error handling, we will define our own error type that
will implements From for a couple of handy other error types.
*/
use std::io::ErrorKind;
use std::fmt::Debug;

use tokio::sync::mpsc::error::SendError;

#[derive(Debug)]
pub enum Error {
    Eof,
    IOError(std::io::Error),
    ClientError(String),
    ServerError(String),
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == ErrorKind::UnexpectedEof {
            Error::Eof
        } else {
            Error::IOError(e)
        }
    }
}

impl<T: Debug> From<SendError<T>> for Error {
    fn from(e: SendError<T>) -> Self {
        Self::ServerError(format!("error sending {:?}", &e.0))
    }
}