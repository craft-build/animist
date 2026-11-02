# F11 — Bedrock `MissingRegion` guidance

## Required workflow

- **Load the `rust-best-practices` skill before writing any code**, and follow
  the followup instructions in
  [the followups README](README.md#instructions-for-every-followup).
- When implementation is finished, **run the review agent** over the full diff,
  and iterate until the verdict is clean.

## Problem

`BedrockError::MissingRegion` renders as a bare statement that the region is
missing. Harnesses surface construction errors verbatim to users, and a user
who hit this has no AWS SDK context: they do not know that the region comes
from the config field, `AWS_REGION`/`AWS_DEFAULT_REGION`, or the shared
config's active profile.

Craft carried its own actionable wording for years (against rig) and dropped
it in the animist cutover rather than re-wrapping the error with a display
string. The message belongs here, where the error is defined.

## Goal

`MissingRegion`'s display names the three fixes in one line, e.g.:

```
no AWS region resolved: set `region` on the provider config, AWS_REGION
or AWS_DEFAULT_REGION, or a `region` in the active AWS profile
```

## Tests

- A display test on the variant asserting each fix appears.

## Out of scope

- Region *resolution* changes (`Bedrock::new` already honors config region,
  profile, and the environment chain).

## Completion note

`BedrockError::MissingRegion` now renders all three fixes in one line: the
provider config `region`, `AWS_REGION`/`AWS_DEFAULT_REGION`, and the active
profile's `region`. A unit display test asserts each source appears, and
`docs/bedrock.md` names the error when no region resolves. Region resolution is
unchanged; release with `fix:`.

Review found one P3 line-width formatting issue, which was fixed; the follow-up
verdict was clean.
