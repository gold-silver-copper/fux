//! A blocking client of one pane: what the command line uses.
use crate::conn::Conn;
use crate::dir::Session;
use crate::sys;
use crate::wire::{Answer, Code, Failure, Hello, MAX_ANSWER, REVISION, Request};
use fuxix::Errno;
use fuxix::poll::{Events, PollFd, poll};
use std::time::{Duration, Instant};

/// How long connecting and greeting may take.
const CONNECT_WAIT: Duration = Duration::from_secs(2);
/// How long an answer that needs no waiting may take.
pub const ANSWER_WAIT: Duration = Duration::from_secs(10);

/// A greeted connection to a pane.
pub struct Client {
    conn: Conn,
    pub hello: Hello,
}

fn deadline(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait).unwrap_or(now)
}

/// A connection to the pane `name`, not yet greeted, and the id its entry
/// names where it names one. A busy pane is tried again until `until`.
pub fn dial(
    session: &Session,
    name: &str,
    until: Instant,
) -> Result<(Conn, Option<crate::wire::PaneId>), Failure> {
    crate::dir::check_name(name)?;
    let dir = session.pane_dir(name);
    loop {
        match sys::connect(&dir) {
            Ok((stream, id)) => return Ok((Conn::new(stream, MAX_ANSWER), id)),
            Err(failure) if failure.code == Code::Busy && Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(failure) if failure.code == Code::Missing => {
                let held = session.claim_held(name).unwrap_or(false);
                let why = match held {
                    true => format!("pane {name} is starting, or cannot be reached"),
                    false => format!("no pane {name}"),
                };
                return Err(Failure::new(
                    if held { Code::Busy } else { Code::Missing },
                    why,
                ));
            }
            Err(failure) => return Err(failure),
        }
    }
}

/// What a greeting must say: this revision, and the id the entry named.
pub fn check_hello(answer: Answer, id: Option<crate::wire::PaneId>) -> Result<Hello, Failure> {
    let answer = answer.accepted()?;
    let Answer::Hello(hello) = answer else {
        return Err(Failure::new(
            Code::Protocol,
            "a greeting answered otherwise",
        ));
    };
    if hello.revision != REVISION {
        let why = format!("the pane speaks revision {}", hello.revision);
        return Err(Failure::new(Code::Protocol, why));
    }
    if id.is_some_and(|id| id != hello.id) {
        let why = format!("the pane answering is {}, not the one named", hello.id);
        return Err(Failure::new(Code::Replaced, why));
    }
    Ok(hello)
}

impl Client {
    /// Connects to the pane `name` and greets it.
    pub fn connect(session: &Session, name: &str) -> Result<Client, Failure> {
        let until = deadline(CONNECT_WAIT);
        loop {
            let (conn, id) = dial(session, name, until)?;
            let mut client = Client {
                conn,
                hello: Hello {
                    revision: REVISION,
                    id: crate::wire::PaneId::default(),
                    labels: Vec::new(),
                    cols: 0,
                    rows: 0,
                    state: crate::wire::State::Running,
                },
            };
            let hello = Request::Hello { revision: REVISION };
            match client.call(&hello, CONNECT_WAIT) {
                Ok(answer) => {
                    client.hello = check_hello(answer, id)?;
                    return Ok(client);
                }
                // A pane out of free slots closes the newcomer: try again.
                Err(failure) if failure.code == Code::Indeterminate && Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(failure) => return Err(failure),
            }
        }
    }

    /// Sends `request` and waits for its answer, at most `wait`. No answer
    /// leaves the request's effect unknown: `indeterminate`, never retried.
    pub fn call(&mut self, request: &Request, wait: Duration) -> Result<Answer, Failure> {
        self.conn.queue(&request.encode())?;
        let until = deadline(wait);
        loop {
            self.conn.flush().map_err(Failure::from)?;
            if let Some(body) = self.conn.receive().map_err(Failure::from)? {
                return Answer::decode(&body);
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                let why = format!("no answer within {wait:?}");
                return Err(Failure::new(Code::Indeterminate, why));
            }
            let events: Events = self.conn.events();
            let mut fds = [PollFd::new(&self.conn.stream, events)];
            match poll(&mut fds, Some(left)) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(errno) => {
                    let why = format!("waiting for the pane: {errno}");
                    return Err(Failure::new(Code::Indeterminate, why));
                }
            }
        }
    }

    /// As [`Client::call`], with a refusal as an error.
    pub fn ask(&mut self, request: &Request, wait: Duration) -> Result<Answer, Failure> {
        self.call(request, wait)?.accepted()
    }
}
