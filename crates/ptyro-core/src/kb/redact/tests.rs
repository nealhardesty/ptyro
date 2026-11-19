use super::*;

fn kb() -> Redactor {
    Redactor::new(vec![("hunter2-value".into(), "<secret:DB_PASS>".into())], &[]).0
}

/// Made-up credentials, one of every shape §25.7 names. (Put together here from parts, so
/// that no scanner takes this file for a leak.)
struct Fakes {
    anthropic: String,
    github: String,
    github_pat: String,
    gitlab: String,
    slack: String,
    aws_id: String,
    aws_secret: String,
    google: String,
    stripe: String,
    jwt: String,
    pem: String,
    password: String,
    opaque: String,
}

impl Fakes {
    fn new() -> Self {
        let body = "Zq3Xv9Lm2Kp7Rt5Wy8Bn4Cd6Fg1Hj0";
        Self {
            anthropic: format!("sk-{}-api03-{body}", "ant"),
            github: format!("gh{}_{body}AbCd12", "p"),
            github_pat: format!("github_{}_11ABCDEFG0{body}_{body}", "pat"),
            gitlab: format!("gl{}-{}", "pat", &body[..20]),
            slack: format!("xox{}-2401234567-2401234568-{}", "b", &body[..24]),
            aws_id: format!("AK{}IOSFODNN7EXAMPLE", "IA"),
            aws_secret: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            google: format!("AI{}SyD-{}_x", "za", &body[..28]),
            stripe: format!("sk_{}_{}", "live", &body[..24]),
            jwt: format!(
                "eyJ{}.eyJ{}.{}",
                "hbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
                "zdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvIn0",
                "SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c"
            ),
            pem: format!(
                "-----BEGIN {k} PRIVATE KEY-----\nMIIEowIBAAKCAQEAqmadeupkeybody0123456789abcdef\n\
                 ZmFrZWZha2VmYWtlZmFrZWZha2VmYWtlZmFrZWZha2U=\n-----END {k} PRIVATE KEY-----",
                k = "RSA"
            ),
            password: "c0rrect-Horse7".into(),
            opaque: "9f8e7d6c5b4a39281706f5e4d3c2b1a0ZZZZYYYY".into(),
        }
    }

    /// What must be found nowhere afterwards: each credential, and its telling middle.
    fn needles(&self) -> Vec<String> {
        vec![
            self.anthropic.clone(),
            self.github.clone(),
            self.github_pat.clone(),
            self.gitlab.clone(),
            self.slack.clone(),
            self.aws_id.clone(),
            self.aws_secret.clone(),
            self.google.clone(),
            self.stripe.clone(),
            self.jwt.clone(),
            self.password.clone(),
            self.opaque.clone(),
            "MIIEowIBAAKCAQEA".into(),
            "ZmFrZWZha2VmYWtl".into(),
            "hunter2".into(),
            "Zq3Xv9Lm2Kp7".into(),
        ]
    }

    fn prose(&self) -> String {
        format!(
            "I set the key to {} and pushed. The old GitHub token was {}, the fine-grained \
             one {}. GitLab's is {}. Slack's bot token: {}. Our AWS id is {} (see the wiki). \
             The Maps key {} is in the page. Stripe: {}. The session was {}.\n\
             The db password is hunter2-value, and the admin password: {}\n\
             Somebody pasted the key:\n{}\nand that was that. A blob {} too.",
            self.anthropic,
            self.github,
            self.github_pat,
            self.gitlab,
            self.slack,
            self.aws_id,
            self.google,
            self.stripe,
            self.jwt,
            self.password,
            self.pem,
            self.opaque
        )
    }

    fn json(&self) -> String {
        format!(
            r#"{{"api_key": "{}", "token":"{}", "aws": {{"access_key": "{}", "aws_secret_access_key": "{}"}},
  "password" : "{}", "client_secret": "{}", "auth": "Bearer {}",
  "url": "postgres://app:{}@db.internal:5432/app", "private_key": "{}",
  "db_password": "hunter2-value"}}"#,
            self.anthropic,
            self.github,
            self.aws_id,
            self.aws_secret,
            self.password,
            self.gitlab,
            self.jwt,
            self.password,
            self.pem.replace('\n', "\\n"),
        )
    }

    fn shell(&self) -> String {
        format!(
            "export ANTHROPIC_API_KEY={}\nexport GITHUB_TOKEN='{}'\nGITLAB_TOKEN=\"{}\" glab mr list\n\
             AWS_ACCESS_KEY_ID={} AWS_SECRET_ACCESS_KEY={} aws s3 ls\n\
             mysql --user=root --password={} app\n\
             curl -H 'Authorization: Bearer {}' https://api.example.com/v1\n\
             curl -H \"Authorization: Bearer {}\" -u admin https://x.example\n\
             fetch https://neal:{}@github.com/acme/app.git\n\
             psql postgres://app:hunter2-value@localhost/app\n\
             curl 'https://maps.example/api?key={}&token={}'\n",
            self.anthropic,
            self.github,
            self.gitlab,
            self.aws_id,
            self.aws_secret,
            self.password,
            self.jwt,
            self.slack,
            self.github,
            self.google,
            self.stripe,
        )
    }

    fn yaml(&self) -> String {
        format!(
            "database:\n  user: app\n  password: {}\n  url: mysql://app:{}@db/app\n\
             github:\n  token: {}\n  api_key: \"{}\"\nslack_token: {}\n\
             stripe:\n  secret: '{}'\nsigning:\n  private_key: |\n    {}\n",
            self.password,
            self.password,
            self.github,
            self.google,
            self.slack,
            self.stripe,
            self.pem.replace('\n', "\n    "),
        )
    }

    fn code(&self) -> String {
        format!(
            "const client = new Client({{ apiKey: \"{}\" }});\n\
             let token = \"{}\";\n\
             password = '{}'\n\
             headers[\"Authorization\"] = \"Bearer {}\";\n\
             static AWS: &str = \"{}\"; // and {}\n\
             conn = connect(host, password=\"{}\", api_key='{}')\n\
             // was: client_secret = \"{}\"\n\
             const KEY: &str = \"{}\";\n\
             let pem = \"{}\";\n",
            self.anthropic,
            self.github,
            self.password,
            self.jwt,
            self.aws_id,
            self.slack,
            self.password,
            self.google,
            self.stripe,
            self.opaque,
            self.pem.replace('\n', "\\n"),
        )
    }
}

#[test]
fn the_rules_compile() {
    assert!(LOOSE.is_some() && STRICT.is_some());
}

/// SPEC §25.16: made-up credentials in every shape of §25.7, in prose, JSON, shell, YAML and
/// code; none survives.
#[test]
fn no_credential_of_the_corpus_survives() {
    let f = Fakes::new();
    let r = kb();
    for (name, text) in [
        ("prose", f.prose()),
        ("json", f.json()),
        ("shell", f.shell()),
        ("yaml", f.yaml()),
        ("code", f.code()),
    ] {
        let out = r.redact(&text);
        for needle in f.needles() {
            if text.contains(&needle) {
                assert!(!out.text.contains(&needle), "{needle} survived in {name}:\n{}", out.text);
            }
        }
        assert!(out.hits >= 6, "{name}: {} hits\n{}", out.hits, out.text);
        // Again changes nothing and counts nothing.
        let again = r.redact(&out.text);
        assert_eq!((again.text.as_str(), again.hits), (out.text.as_str(), 0), "{name}");
    }
}

#[test]
fn what_is_left_says_what_was_there() {
    let f = Fakes::new();
    let out = kb().redact(&f.shell()).text;
    assert!(out.contains("export ANTHROPIC_API_KEY=<redacted>\n"), "{out}");
    assert!(out.contains("export GITHUB_TOKEN='<redacted>'\n"), "{out}");
    assert!(out.contains("mysql --user=root --password=<redacted> app"), "{out}");
    assert!(out.contains("'Authorization: Bearer <redacted>' https://api.example.com/v1"), "{out}");
    assert!(out.contains("https://neal:<redacted>@github.com/acme/app.git"), "{out}");
    assert!(out.contains("postgres://app:<secret:DB_PASS>@localhost/app"), "{out}");
    assert!(out.contains("?key=<redacted>&token=<redacted>'"), "{out}");
    let out = kb().redact(&f.json()).text;
    assert!(out.contains(r#""password" : "<redacted>""#), "{out}");
    assert!(out.contains(r#""private_key": "<redacted>""#), "{out}");
    assert!(out.contains(r#""db_password": "<secret:DB_PASS>""#), "{out}");
    let out = kb().redact(&f.prose()).text;
    assert!(out.contains("pasted the key:\n<redacted>\nand that was that"), "{out}");
}

/// The other half: what looks a little like a secret and is not. The knowledge base is read by
/// people; rules that eat ordinary text would make it useless.
#[test]
fn look_alikes_are_left_alone() {
    let text = "\
commit 3f2a9c1e8b7d6a5f4e3d2c1b0a9f8e7d6c5b4a39 fixed it (also 3f2a9c1e)
sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
request 123e4567-e89b-12d3-a456-426614174000 failed twice
<img src=\"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk\">
She was the bearer of bad news, and the bearer token flow is in RFC 6750.
token = get_token()
let token = self.next_token()?;
password = getpass.getpass(\"Password: \")
client = Client(token=token, password=self.password, secret=None)
fn login(password: &str, token: String) -> Result<Token> {
max_tokens=4096 and token_count = 12 and tokens: 8000
The password: is kept in the keychain. A token: one word of a query.
export TOKEN=$GITHUB_TOKEN and api_key=${API_KEY} and password=<your password>
Authorization: Bearer $TOKEN, or Bearer <token>, or Bearer {token}
see https://example.com:8443/path and http://localhost:3000/a@b and git@github.com:acme/app.git
test_that_a_very_long_function_name_from_2024_11_19_still_parses_utf8
crates/ptyro-core/src/kb/a_rather_long_module_name_for_tests_2026_11_19.rs
the task-management-system-design-document and risk-assessment-2024-final
sk-learn is a library; AKIA is a prefix; a normal sentence with src/main.rs and 1234 stays
";
    let out = kb().redact(text);
    assert_eq!(out.text, text);
    assert_eq!(out.hits, 0);
}

/// What a chat's titler is sent (it leaves the machine): as before the rules moved here, and
/// every doubt goes.
#[test]
fn for_a_model_every_doubt_is_redacted() {
    let secrets = vec![("hunter2-value".to_owned(), "<secret:DB_PASS>".to_owned())];
    let text = "connect hunter2-value\n\
                export GITHUB_TOKEN=abc123 and --password=pw\n\
                key sk-ant-api03-aaaaaaaaaaaaaaaaaaaa here\n\
                curl -H 'Authorization: Bearer eyJhbGciOi' x\n\
                blob 9f8e7d6c5b4a39281706f5e4d3c2b1a0ZZZZYYYY end\n\
                a normal sentence with src/main.rs and 1234 stays";
    let r = for_model(text, &secrets);
    assert!(r.contains("connect <secret:DB_PASS>"), "{r}");
    assert!(r.contains("GITHUB_TOKEN=<redacted>") && r.contains("--password=<redacted>"), "{r}");
    assert!(r.contains("key <redacted> here"), "{r}");
    assert!(r.contains("Bearer <redacted>' x"), "{r}");
    assert!(r.contains("blob <redacted> end"), "{r}");
    assert!(r.contains("a normal sentence with src/main.rs and 1234 stays"), "{r}");
    for leaked in ["hunter2", "abc123", "sk-ant", "eyJhbG", "9f8e7d"] {
        assert!(!r.contains(leaked), "{leaked} in {r}");
    }
    // Stricter than the knowledge base's: a hash, any word after Bearer, a key with the word
    // anywhere in it.
    let r = for_model(
        "at 3f2a9c1e8b7d6a5f4e3d2c1b0a9f8e7d6c5b4a39, bearer of news, token_count=12 token:abc",
        &[],
    );
    assert_eq!(r, "at <redacted>, bearer <redacted> news, token_count=<redacted> token:<redacted>");
    assert_eq!(for_model(&r, &[]), r);
}

#[test]
fn a_pem_block_goes_whole_and_a_cut_off_one_too() {
    let r = kb();
    let two = "a\n-----BEGIN CERTIFICATE-----\nMIIB\nAAAA\n-----END CERTIFICATE-----\nb\n\
               -----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNz\n-----END OPENSSH PRIVATE KEY-----\nc";
    let out = r.redact(two);
    assert_eq!((out.text.as_str(), out.hits), ("a\n<redacted>\nb\n<redacted>\nc", 2));
    // Cut off: the header and the base64 under it, and what follows stays.
    let cut = "key:\n-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\nBKcwggSjAgEA\n\nThen more text.";
    let out = r.redact(cut);
    assert_eq!(out.text, "key:\n<redacted>\n\nThen more text.");
    // Many headers and no end: every one goes, and it doesn't take long.
    let many = "-----BEGIN PRIVATE KEY-----\nMIIE\n".repeat(20_000);
    let t = std::time::Instant::now();
    let out = r.redact(&many);
    assert_eq!(out.hits, 20_000);
    assert!(!out.text.contains("MIIE"));
    assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
}

#[test]
fn the_users_own_patterns() {
    let (r, errors) = Redactor::new(
        Vec::new(),
        &[
            "ACME-[0-9]{6}".into(),
            "(unclosed".into(),
            "  ".into(),
            r"(?i)internal\.acme\.dev".into(),
        ],
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("(unclosed"), "{errors:?}");
    let out = r.redact("ticket ACME-123456 on Internal.Acme.Dev, not ACME-12");
    assert_eq!(out.text, "ticket <redacted> on <redacted>, not ACME-12");
    assert_eq!(out.hits, 2);
    assert_eq!(r.redact(&out.text).hits, 0);
}

#[test]
fn a_secret_too_short_to_look_for_is_not_looked_for() {
    let (r, _) = Redactor::new(vec![("abc".into(), "<secret:X>".into())], &[]);
    assert_eq!(r.redact("abc abcabc").text, "abc abcabc");
}

/// It runs over every transcript and file: 1 MiB of ordinary text, with a secret now and
/// then. Prints the time (`make bench-kb-redact` runs it optimised); the bound is loose enough
/// for a debug build on a busy machine.
#[test]
fn a_mebibyte_takes_milliseconds() {
    let f = Fakes::new();
    let para = "The daemon keeps one connection per client and answers each request in order; \
                see crates/ptyro-daemon/src/conn.rs:120 and `fn handle(&self, req: Request)`.\n\
                let n = items.iter().filter(|i| i.kind == Kind::Session).count(); // 3f2a9c1e\n\
                - [x] **KB1.1** `kb.db`: the schema of §25.4, max_tokens=4096, token = get_token()\n\n";
    let mut text = String::with_capacity((1 << 20) + 4096);
    let mut n = 0;
    while text.len() < 1 << 20 {
        text.push_str(para);
        n += 1;
        if n % 400 == 0 {
            text.push_str(&format!("export GITHUB_TOKEN={}\n", f.github));
        }
    }
    let r = kb();
    r.redact("warm"); // the rules compile once a process
    let t = std::time::Instant::now();
    let out = r.redact(&text);
    let took = t.elapsed();
    println!(
        "redaction: {:.1} ms for {} KiB, {} hits",
        took.as_secs_f64() * 1000.0,
        text.len() / 1024,
        out.hits
    );
    assert_eq!(out.hits, n / 400);
    assert!(!out.text.contains(&f.github));
    assert!(took.as_secs() < 20, "{took:?}");
}
