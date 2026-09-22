# ADR-0014: Distributed execution requires an explicit trust boundary

Status: Proposed

## Context

Remote workers, distributed leases, daemon scheduling, organization policy,
team collaboration, and marketplace execution cross the local process and
workspace boundary. Local event, attempt, capability, and lease contracts do
not define remote identity, transport authentication, clock skew, secret
delivery, compatibility, or partition recovery.

## Decision

No P3 distributed feature may ship until a successor to this proposal is
accepted with concrete protocols for identity, mutually authenticated
transport, capability attenuation, secret brokering, event replay, lease
epochs and clocks, version negotiation, revocation, and recovery.

Remote content remains untrusted. A remote worker receives only an explicit
attempt, isolated workspace assignment, and attenuated capabilities. It cannot
append canonical events directly; an authenticated local coordinator validates
and records results. Late or duplicate workers are fenced by attempt and lease
epoch.

## Consequences

- The local runtime remains the sole launch boundary for P1/P2 work.
- Product surfaces label remote/platform work deferred.
- A future implementation ADR includes adversarial replay, clock-skew,
  credential-confusion, partition, revocation, and compatibility tests.

## Alternatives

- Reusing local process identity remotely was rejected because it has no
  authenticated principal or revocation story.
- Letting workers write the canonical event store was rejected because it
  bypasses the coordinator's policy and ordering boundary.

## Gate

This record must be reviewed and accepted by `@suiflex/maintainers`, then
superseded by an implementation ADR with passing trust-boundary tests before
any P3 distributed feature is described as operational.
