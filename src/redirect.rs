//! Redirect Handling
//!
//! By default, a `Client` will automatically handle HTTP redirects, having a
//! maximum redirect chain of 10 hops. To customize this behavior, a
//! `redirect::Policy` can be used with a `ClientBuilder`.

use std::fmt;
use std::{error::Error as StdError, sync::Arc};

use crate::core::StatusCode;
use crate::header::{AUTHORIZATION, COOKIE, HeaderMap, PROXY_AUTHORIZATION, WWW_AUTHENTICATE};
use http::Method;

use crate::Url;

/// A type that controls the policy on how to handle the following of redirects.
///
/// The default value will catch redirect loops, and has a maximum of 10
/// redirects it will follow in a chain before returning an error.
///
/// - `limited` can be used have the same as the default behavior, but adjust
///   the allowed maximum redirect hops in a chain.
/// - `none` can be used to disable all redirect behavior.
/// - `custom` can be used to create a customized policy.
#[derive(Clone)]
pub struct Policy {
    inner: PolicyKind,
}

/// A type that holds information on the next request and previous requests
/// in redirect chain.
#[derive(Debug)]
pub struct Attempt<'a> {
    status: StatusCode,
    next_method: &'a Method,
    next: &'a Url,
    previous_method: &'a Method,
    previous: &'a [Url],
}

/// An action to perform when a redirect status code is found.
#[derive(Debug)]
pub struct Action {
    inner: ActionKind,
}

impl Policy {
    /// Create a `Policy` with a maximum number of redirects.
    ///
    /// An `Error` will be returned if the max is reached.
    pub fn limited(max: usize) -> Self {
        Self {
            inner: PolicyKind::Limit(max),
        }
    }

    /// Create a `Policy` that does not follow any redirect.
    pub fn none() -> Self {
        Self {
            inner: PolicyKind::None,
        }
    }

    /// Create a custom `Policy` using the passed function.
    ///
    /// # Note
    ///
    /// The default `Policy` handles a maximum loop
    /// chain, but the custom variant does not do that for you automatically.
    /// The custom policy should have some way of handling those.
    ///
    /// Information on the next request and previous requests can be found
    /// on the [`Attempt`] argument passed to the closure.
    ///
    /// Actions can be conveniently created from methods on the
    /// [`Attempt`].
    ///
    /// # Example
    ///
    /// ```rust
    /// # use rquest::{Error, redirect};
    /// #
    /// # fn run() -> Result<(), Error> {
    /// let custom = redirect::Policy::custom(|attempt| {
    ///     if attempt.previous().len() > 5 {
    ///         attempt.error("too many redirects")
    ///     } else if attempt.url().host_str() == Some("example.domain") {
    ///         // prevent redirects to 'example.domain'
    ///         attempt.stop()
    ///     } else {
    ///         attempt.follow()
    ///     }
    /// });
    /// let client = rquest::Client::builder()
    ///     .redirect(custom)
    ///     .build()?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// [`Attempt`]: struct.Attempt.html
    pub fn custom<T>(policy: T) -> Self
    where
        T: Fn(Attempt) -> Action + Send + Sync + 'static,
    {
        Self {
            inner: PolicyKind::Custom(Arc::new(policy)),
        }
    }

    /// Apply this policy to a given [`Attempt`] to produce a [`Action`].
    ///
    /// # Note
    ///
    /// This method can be used together with `Policy::custom()`
    /// to construct one `Policy` that wraps another.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use rquest::{Error, redirect};
    /// #
    /// # fn run() -> Result<(), Error> {
    /// let custom = redirect::Policy::custom(|attempt| {
    ///     eprintln!("{}, Location: {:?}", attempt.status(), attempt.url());
    ///     redirect::Policy::default().redirect(attempt)
    /// });
    /// # Ok(())
    /// # }
    /// ```
    pub fn redirect(&self, attempt: Attempt) -> Action {
        match self.inner {
            PolicyKind::Custom(ref custom) => custom(attempt),
            PolicyKind::Limit(max) => {
                // The first URL in the previous is the initial URL and not a redirection. It needs to be excluded.
                if attempt.previous.len() > max {
                    attempt.error(TooManyRedirects)
                } else {
                    attempt.follow()
                }
            }
            PolicyKind::None => attempt.stop(),
        }
    }

    pub(crate) fn check(
        &self,
        status: StatusCode,
        next_method: &Method,
        next: &Url,
        previous_method: &Method,
        previous: &[Url],
    ) -> ActionKind {
        self.redirect(Attempt {
            status,
            next_method,
            next,
            previous_method,
            previous,
        })
        .inner
    }

    pub(crate) fn remove_sensitive_headers(headers: &mut HeaderMap, next: &Url, previous: &[Url]) {
        if let Some(previous) = previous.last() {
            let cross_host = next.host_str() != previous.host_str()
                || next.port_or_known_default() != previous.port_or_known_default();
            if cross_host {
                headers.remove(AUTHORIZATION);
                headers.remove(COOKIE);
                headers.remove("cookie2");
                headers.remove(PROXY_AUTHORIZATION);
                headers.remove(WWW_AUTHENTICATE);
            }
        }
    }
}

impl Default for Policy {
    fn default() -> Policy {
        // Keep `is_default` in sync
        Policy::limited(10)
    }
}

impl Attempt<'_> {
    /// Get the type of redirect.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// Get the method for the next request, after applying redirection logic.
    pub fn next_method(&self) -> &Method {
        self.next_method
    }

    /// Get the next URL to redirect to.
    pub fn url(&self) -> &Url {
        self.next
    }

    /// Get the method for the previous request, before redirection.
    pub fn previous_method(&self) -> &Method {
        self.previous_method
    }

    /// Get the list of previous URLs that have already been requested in this chain.
    pub fn previous(&self) -> &[Url] {
        self.previous
    }
    /// Returns an action meaning rquest should follow the next URL.
    pub fn follow(self) -> Action {
        Action {
            inner: ActionKind::Follow,
        }
    }

    /// Returns an action meaning rquest should not follow the next URL.
    ///
    /// The 30x response will be returned as the `Ok` result.
    pub fn stop(self) -> Action {
        Action {
            inner: ActionKind::Stop,
        }
    }

    /// Returns an action failing the redirect with an error.
    ///
    /// The `Error` will be returned for the result of the sent request.
    pub fn error<E: Into<Box<dyn StdError + Send + Sync>>>(self, error: E) -> Action {
        Action {
            inner: ActionKind::Error(error.into()),
        }
    }
}

#[derive(Clone)]
enum PolicyKind {
    Custom(Arc<dyn Fn(Attempt) -> Action + Send + Sync + 'static>),
    Limit(usize),
    None,
}

impl fmt::Debug for Policy {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Policy").field(&self.inner).finish()
    }
}

impl fmt::Debug for PolicyKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            PolicyKind::Custom(..) => f.pad("Custom"),
            PolicyKind::Limit(max) => f.debug_tuple("Limit").field(&max).finish(),
            PolicyKind::None => f.pad("None"),
        }
    }
}

// pub(crate)

#[derive(Debug)]
pub(crate) enum ActionKind {
    Follow,
    Stop,
    Error(Box<dyn StdError + Send + Sync>),
}

#[derive(Debug)]
struct TooManyRedirects;

impl fmt::Display for TooManyRedirects {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("too many redirects")
    }
}

impl StdError for TooManyRedirects {}

#[test]
fn test_redirect_policy_limit() {
    let policy = Policy::default();
    let next = Url::parse("http://x.y/z").unwrap();
    let mut previous = (0..=9)
        .map(|i| Url::parse(&format!("http://a.b/c/{}", i)).unwrap())
        .collect::<Vec<_>>();

    match policy.check(
        StatusCode::FOUND,
        &Method::GET,
        &next,
        &Method::GET,
        &previous,
    ) {
        ActionKind::Follow => (),
        other => panic!("unexpected {:?}", other),
    }

    previous.push(Url::parse("http://a.b.d/e/33").unwrap());

    match policy.check(
        StatusCode::FOUND,
        &Method::GET,
        &next,
        &Method::GET,
        &previous,
    ) {
        ActionKind::Error(err) if err.is::<TooManyRedirects>() => (),
        other => panic!("unexpected {:?}", other),
    }
}

#[test]
fn test_redirect_policy_limit_to_0() {
    let policy = Policy::limited(0);
    let next = Url::parse("http://x.y/z").unwrap();
    let previous = vec![Url::parse("http://a.b/c").unwrap()];

    match policy.check(
        StatusCode::FOUND,
        &Method::GET,
        &next,
        &Method::GET,
        &previous,
    ) {
        ActionKind::Error(err) if err.is::<TooManyRedirects>() => (),
        other => panic!("unexpected {:?}", other),
    }
}

#[test]
fn test_redirect_policy_custom() {
    let policy = Policy::custom(|attempt| {
        if attempt.url().host_str() == Some("foo") {
            attempt.stop()
        } else {
            attempt.follow()
        }
    });

    let next = Url::parse("http://bar/baz").unwrap();
    match policy.check(StatusCode::FOUND, &Method::GET, &next, &Method::GET, &[]) {
        ActionKind::Follow => (),
        other => panic!("unexpected {:?}", other),
    }

    let next = Url::parse("http://foo/baz").unwrap();
    match policy.check(StatusCode::FOUND, &Method::GET, &next, &Method::GET, &[]) {
        ActionKind::Stop => (),
        other => panic!("unexpected {:?}", other),
    }
}

#[test]
fn test_redirect_custom_policy_methods() {
    let policy = Policy::custom(|attempt| {
        let next = attempt.next_method();
        if next != Method::HEAD {
            panic!("unexpected next method {:?}", next);
        }
        let prev = attempt.previous_method();
        if prev != Method::PUT {
            panic!("unexpected previous method {:?}", prev);
        }
        attempt.stop()
    });

    let next = Url::parse("http://bar/baz").unwrap();
    let res = policy.check(StatusCode::FOUND, &Method::HEAD, &next, &Method::PUT, &[]);
    assert!(matches!(res, ActionKind::Stop));
}

#[test]
fn test_remove_sensitive_headers() {
    use crate::core::header::{ACCEPT, AUTHORIZATION, COOKIE, HeaderValue};

    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
    headers.insert(AUTHORIZATION, HeaderValue::from_static("let me in"));
    headers.insert(COOKIE, HeaderValue::from_static("foo=bar"));

    let next = Url::parse("http://initial-domain.com/path").unwrap();
    let mut prev = vec![Url::parse("http://initial-domain.com/new_path").unwrap()];
    let mut filtered_headers = headers.clone();

    Policy::remove_sensitive_headers(&mut headers, &next, &prev);
    assert_eq!(headers, filtered_headers);

    prev.push(Url::parse("http://new-domain.com/path").unwrap());
    filtered_headers.remove(AUTHORIZATION);
    filtered_headers.remove(COOKIE);

    Policy::remove_sensitive_headers(&mut headers, &next, &prev);
    assert_eq!(headers, filtered_headers);
}
