# Contributing to _h2_ #

:balloon: Thanks for your help improving the project!

## Getting Help ##

If you have a question about the h2 library or have encountered problems using it, you may
[file an issue][issue] or ask ask a question on the [Tokio Gitter][gitter].

## Submitting a Pull Request ##

Do you have an improvement?

1. Submit an [issue][issue] describing your proposed change.
2. We will try to respond to your issue promptly.
3. Fork this repo, develop and test your code changes. See the project's [README](README.md) for further information about working in this repository.
4. Submit a pull request against this repo's `master` branch.
6. Your branch may be merged once all configured checks pass, including:
    - Code review has been completed.
    - The branch has passed tests in CI.

## Committing ##

We prefer squash or rebase commits so that all changes from a branch are
committed to master as a single commit. All pull requests are squashed when
merged, but rebasing prior to merge gives you better control over the commit
message.

### Commit messages ###

Finalized commit messages should be in the following format:

```
Subject

Problem

Solution

Validation
```

#### Subject ####

- one line, <= 50 characters
- describe what is done; not the result
- use the active voice
- capitalize first word and proper nouns
- do not end in a period — this is a title/subject
- reference the github issue by number

##### Examples #####

```
bad: server disconnects should cause dst client disconnects.
good: Propagate disconnects from source to destination
```

```
bad: support tls servers
good: Introduce support for server-side TLS (#347)
```

#### Problem ####

Explain the context and why you're making that change.  What is the problem
you're trying to solve? In some cases there is not a problem and this can be
thought of as being the motivation for your change.

#### Solution ####

Describe the modifications you've made.

#### Validation ####

Describe the testing you've done to validate your change.  Performance-related
changes should include before- and after- benchmark results.

[issue]: https://github.com/carllerche/h2/issues/new
[gitter]: https://gitter.im/tokio-rs/tokio
[rustfmt]: https://github.com/rust-lang-nursery/rustfmt
