# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.1.x   | Yes       |

## Reporting a Vulnerability

Please report security vulnerabilities to spicyzhug@gmail.com.

Do NOT open a public GitHub issue for security vulnerabilities.

We will acknowledge receipt within 48 hours and provide a detailed
response within 5 business days.

## Security Practices

- Zero `unsafe` code in production
- All dependencies audited via `cargo-deny`
- Fuzz testing via `cargo-fuzz` / libFuzzer
- HMAC-SHA256 webhook validation with constant-time comparison
- Kubernetes TokenReview + SubjectAccessReview for agent auth
- Non-root container with dropped capabilities
- Read-only root filesystem
- Seccomp RuntimeDefault profile
