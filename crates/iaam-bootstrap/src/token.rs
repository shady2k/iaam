//! `iaam token` and `iaam claim`: the console's credential commands.
//!
//! Everything here goes through the [`TokenAdmin`] port, and through nothing
//! else: issuance exists only behind that port (see its doc comment), so this
//! module names the port's operations and prints their results. Assembling a
//! token record here would be a second implementation of credential issuance —
//! the old debt this module exists to keep paid.
//!
//! A token is shown **once**. The commands print the secret on standard
//! output and one advisory line on standard error, so a pipeline that takes
//! only stdout takes exactly the token.

use iaam_app::ports::{Scope, SoleOwner, TokenAdmin, TokenView};

use clap::{Subcommand, ValueEnum};
use iaam_core::ids::OwnerId;

#[derive(Debug, Subcommand)]
pub(crate) enum TokenCommand {
    /// Issue a token for the existing sole owner.
    Issue {
        /// Whom the token is for: `owner`, `agent` or `read-only`.
        #[arg(value_enum)]
        scope: TokenScopeArg,
        /// A name to recognise the token by later (`iaam token list`,
        /// `iaam token revoke`). Without it the token is named after its
        /// scope and today's date, e.g. `agent 2026-09-30`.
        #[arg(long)]
        label: Option<String>,
    },
    /// List this instance's tokens: active first, then revoked.
    List,
    /// Revoke one active token, named by its label or its id.
    Revoke {
        /// The token's label, or its id when the label is ambiguous.
        /// Labels and ids are shown by `iaam token list`.
        target: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum TokenScopeArg {
    Owner,
    Agent,
    ReadOnly,
}

impl From<TokenScopeArg> for Scope {
    fn from(scope: TokenScopeArg) -> Self {
        match scope {
            TokenScopeArg::Owner => Self::Owner,
            TokenScopeArg::Agent => Self::Agent,
            TokenScopeArg::ReadOnly => Self::ReadOnly,
        }
    }
}

/// The scope's name in this command's vocabulary: the same word the
/// `token issue` argument takes. (`read-only` here; the wire spells it
/// `read_only` — that difference belongs to the transport, not to this one.)
pub(crate) const fn scope_text(scope: Scope) -> &'static str {
    match scope {
        Scope::Owner => "owner",
        Scope::Agent => "agent",
        Scope::ReadOnly => "read-only",
    }
}

/// The line printed beside a token shown once: where it goes, and that no
/// command, log or database can produce it a second time.
pub(crate) const SHOWN_ONCE: &str = "shown only now: put it in the owner's password manager or the agent's \
     configuration; it cannot be shown again";

/// The label a claiming or issuing command accepts: free text without
/// control characters. The list prints one label per line, so a control
/// character in a stored label would forge a line there — the refusal says
/// which characters are the problem.
fn checked_label(label: String) -> Result<String, Box<dyn std::error::Error>> {
    let mut controls: Vec<char> = label.chars().filter(|c| c.is_control()).collect();
    controls.sort();
    controls.dedup();
    if controls.is_empty() {
        return Ok(label);
    }
    let listed = controls
        .iter()
        .map(|c| format!("U+{:04X}", *c as u32))
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "label {:?} is invalid: it contains the control character(s) {listed}; \
         a label must not contain control characters, because `iaam token list` \
         prints one label per line",
        label
    )
    .into())
}

/// The instance's single owner, or the refusal that names the command which
/// creates one.
///
/// `issue`, `list` and `revoke` all act on the sole owner; a database with
/// several owners is corruption in a single-user system, and choosing among
/// them is not this command's decision.
async fn sole_owner_or_refuse(
    admin: &dyn TokenAdmin,
) -> Result<OwnerId, Box<dyn std::error::Error>> {
    match admin.sole_owner().await? {
        SoleOwner::Single(owner) => Ok(owner),
        SoleOwner::None => Err("instance has no owner: run `iaam claim` first".into()),
        SoleOwner::Several => Err(
            "multiple owners in database: choosing which one should receive \
                    a token is impossible. These are signs of corruption in \
                    a single-user system — inspect the database, not the command"
                .into(),
        ),
    }
}

/// Claim an instance and issue its first owner token, printing it once.
///
/// The label is optional: a claimed instance has exactly one owner token,
/// and `owner` says what it is. The decision that the instance is unclaimed
/// and the minting of the token are one operation behind the port — this
/// function only supplies the label.
pub(crate) async fn claim_owner(
    admin: &dyn TokenAdmin,
    label: Option<String>,
) -> Result<String, Box<dyn std::error::Error>> {
    let label = checked_label(label.unwrap_or_else(|| "owner".to_owned()))?;
    let issued = admin.claim_owner(label).await?;
    Ok(issued.token)
}

/// Issue a token for the existing sole owner, printing it once.
///
/// Without `--label` the token is named after its scope and the day it was
/// issued (`agent 2026-09-30`): readable in `iaam token list`, and honest
/// about when it appeared. Two tokens may end up with one label — `revoke`
/// refuses an ambiguous label and offers the ids instead.
pub(crate) async fn issue_token(
    admin: &dyn TokenAdmin,
    scope: Scope,
    label: Option<String>,
    today: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let owner = sole_owner_or_refuse(admin).await?;
    let label = match label {
        Some(label) => checked_label(label)?,
        None => format!("{} {today}", scope_text(scope)),
    };
    let issued = admin.issue_token(owner, label, scope).await?;
    Ok(issued.token)
}

/// Every token of this instance, active first, then the revoked ones —
/// "when did this token stop working" is a question the list answers.
/// Neither the secret nor its hash exists in the port's answer, so neither
/// can appear here.
pub(crate) async fn list_tokens(
    admin: &dyn TokenAdmin,
) -> Result<String, Box<dyn std::error::Error>> {
    let owner = sole_owner_or_refuse(admin).await?;
    let tokens = admin.list_tokens(owner).await?;
    Ok(render_listing(&tokens))
}

/// One line per token: the id, the label, the scope, when it was created,
/// and `active` or `revoked <time>`. Active tokens come first, then the
/// revoked ones, each in the store's own order — the list reads as "what
/// works now", with the history after it. The id is on every line so an
/// ambiguous label in `iaam token revoke` always has its answer at hand.
fn render_listing(tokens: &[TokenView]) -> String {
    let (active, revoked): (Vec<&TokenView>, Vec<&TokenView>) =
        tokens.iter().partition(|token| token.revoked_at.is_none());
    let mut lines = Vec::new();
    for token in active.into_iter().chain(revoked) {
        let state = match &token.revoked_at {
            None => "active".to_owned(),
            Some(at) => format!("revoked {at}"),
        };
        lines.push(format!(
            "{}  {}  {}  created {}  {}",
            token.id,
            shown_label(&token.label),
            scope_text(token.scope),
            token.created_at,
            state
        ));
    }
    let mut text = lines.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

/// The label as the list prints it: control characters escaped, so a label
/// that arrived over HTTP cannot forge a line here. Everything else is
/// printed exactly as it is.
fn shown_label(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// Revoke one active token by label or by id, and print which one.
///
/// A target that parses as a UUID is an id when some token carries it;
/// when none does, it is looked up as a label, under the same rule —
/// labels are the owner's free text and may look exactly like a UUID. A
/// label naming several active tokens is refused with their ids: two
/// tokens may share a label (the default label is the scope and the day),
/// and revoking "whichever one" is not a decision the command makes for
/// the owner. An unknown label or id is refused naming what was looked
/// for. A revoked token stays revoked: revoking it again is refused, not
/// idempotent silence, so the command's answer always says what happened.
pub(crate) async fn revoke_token(
    admin: &dyn TokenAdmin,
    target: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let owner = sole_owner_or_refuse(admin).await?;
    let tokens = admin.list_tokens(owner).await?;
    let id = uuid::Uuid::parse_str(target)
        .ok()
        .filter(|id| tokens.iter().any(|token| token.id == *id));
    let Some(token) = id.and_then(|id| tokens.iter().find(|token| token.id == id)) else {
        return revoke_by_label(admin, owner, &tokens, target).await;
    };
    if let Some(at) = &token.revoked_at {
        return Err(format!(
            "token \"{}\" ({}) was already revoked at {at}; nothing to revoke",
            token.label, token.id
        )
        .into());
    }
    revoke_and_report(admin, owner, token).await
}

/// Resolves a label to the one active token carrying it, or refuses.
///
/// Several active tokens under one label are refused with their ids: the
/// default label is the scope and the day, so two tokens of one day can
/// share a label, and the choice between them is the owner's, made with
/// `iaam token revoke <id>`.
async fn revoke_by_label(
    admin: &dyn TokenAdmin,
    owner: OwnerId,
    tokens: &[TokenView],
    label: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let active: Vec<&TokenView> = tokens
        .iter()
        .filter(|token| token.label == label && token.revoked_at.is_none())
        .collect();
    match active.len() {
        1 => revoke_and_report(admin, owner, active[0]).await,
        0 => {
            let revoked: Vec<&TokenView> = tokens
                .iter()
                .filter(|token| token.label == label && token.revoked_at.is_some())
                .collect();
            if revoked.is_empty() {
                Err(format!(
                    "no token named \"{label}\": labels and ids come from `iaam token list`"
                )
                .into())
            } else {
                let listed = revoked
                    .iter()
                    .map(|token| {
                        format!(
                            "  {}  revoked {}",
                            token.id,
                            token.revoked_at.as_deref().unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Err(format!(
                    "no active token named \"{label}\": every token with this \
                     label is already revoked:\n{listed}"
                )
                .into())
            }
        }
        _ => {
            let listed = active
                .iter()
                .map(|token| format!("  {}  created {}", token.id, token.created_at))
                .collect::<Vec<_>>()
                .join("\n");
            Err(format!(
                "label \"{label}\" names {} active tokens; revoke one by its id:\n{listed}",
                active.len()
            )
            .into())
        }
    }
}

/// Revokes the token and names what was revoked: the label, the scope and
/// the id, so the answer stays true when two tokens share a label.
async fn revoke_and_report(
    admin: &dyn TokenAdmin,
    owner: OwnerId,
    token: &TokenView,
) -> Result<String, Box<dyn std::error::Error>> {
    admin.revoke_token(owner, token.id).await?;
    Ok(format!(
        "revoked: {} ({}, id {})",
        token.label,
        scope_text(token.scope),
        token.id
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        SHOWN_ONCE, TokenCommand, TokenScopeArg, claim_owner, issue_token, list_tokens,
        revoke_token, scope_text, sole_owner_or_refuse,
    };
    use crate::{Cli, Command};
    use clap::Parser;
    use iaam_app::adapters::sqlite::SqliteAdapter;
    use iaam_app::ports::{Scope, Store, TokenAdmin};

    fn adapter() -> SqliteAdapter {
        SqliteAdapter::new(iaam_store::SqliteStore::open_in_memory().unwrap())
    }

    // --- The command line -------------------------------------------------

    #[test]
    fn cli_parses_token_issue_with_positional_scope_and_optional_label() {
        let cli = Cli::try_parse_from(["iaam", "token", "issue", "owner"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Token {
                command: TokenCommand::Issue {
                    scope: TokenScopeArg::Owner,
                    label: None,
                }
            }
        ));

        let cli = Cli::try_parse_from(["iaam", "token", "issue", "read-only", "--label", "Main"])
            .unwrap();
        assert!(matches!(
            cli.command,
            Command::Token {
                command: TokenCommand::Issue {
                    scope: TokenScopeArg::ReadOnly,
                    label: Some(_),
                }
            }
        ));

        let cli = Cli::try_parse_from(["iaam", "token", "issue", "agent"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Token {
                command: TokenCommand::Issue {
                    scope: TokenScopeArg::Agent,
                    ..
                }
            }
        ));
    }

    #[test]
    fn cli_refuses_the_dropped_scope_flag() {
        let error = Cli::try_parse_from([
            "iaam", "token", "issue", "--scope", "owner", "--label", "Main",
        ])
        .unwrap_err();
        assert!(error.to_string().contains("unexpected argument"), "{error}");
    }

    #[test]
    fn cli_refuses_token_issue_without_a_scope() {
        let error = Cli::try_parse_from(["iaam", "token", "issue", "--label", "Main"]).unwrap_err();
        assert!(error.to_string().contains("<SCOPE>"), "{error}");
    }

    #[test]
    fn cli_parses_token_list_and_token_revoke() {
        let cli = Cli::try_parse_from(["iaam", "token", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Token {
                command: TokenCommand::List
            }
        ));

        let cli = Cli::try_parse_from(["iaam", "token", "revoke", "home agent"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Token {
                command: TokenCommand::Revoke { .. }
            }
        ));
    }

    #[test]
    fn cli_parses_claim_without_a_label() {
        let cli = Cli::try_parse_from(["iaam", "claim"]).unwrap();
        assert!(matches!(cli.command, Command::Claim { label: None }));

        let cli = Cli::try_parse_from(["iaam", "claim", "--label", "console"]).unwrap();
        assert!(matches!(cli.command, Command::Claim { label: Some(_) }));
    }

    #[test]
    fn the_scope_is_spelled_as_the_command_takes_it() {
        assert_eq!(scope_text(Scope::Owner), "owner");
        assert_eq!(scope_text(Scope::Agent), "agent");
        assert_eq!(scope_text(Scope::ReadOnly), "read-only");
    }

    #[test]
    fn the_shown_once_line_says_it_is_the_only_showing() {
        assert!(SHOWN_ONCE.contains("shown only now"));
        assert!(SHOWN_ONCE.contains("cannot be shown again"));
    }

    // --- Defaults -----------------------------------------------------------

    #[tokio::test]
    async fn issue_without_a_label_names_the_scope_and_the_day() {
        let admin = adapter();
        claim_owner(&admin, Some("console".to_owned()))
            .await
            .unwrap();
        issue_token(&admin, Scope::Agent, None, "2026-09-30")
            .await
            .unwrap();
        let owner = sole_owner_or_refuse(&admin).await.unwrap();
        let tokens = admin.list_tokens(owner).await.unwrap();
        assert!(
            tokens.iter().any(|token| token.label == "agent 2026-09-30"),
            "{tokens:?}"
        );
    }

    #[tokio::test]
    async fn claim_without_a_label_names_the_owner() {
        let admin = adapter();
        claim_owner(&admin, None).await.unwrap();
        let owner = sole_owner_or_refuse(&admin).await.unwrap();
        let tokens = admin.list_tokens(owner).await.unwrap();
        assert_eq!(tokens.len(), 1, "{tokens:?}");
        assert_eq!(tokens[0].label, "owner");
    }

    #[tokio::test]
    async fn an_explicit_label_is_kept_verbatim() {
        let admin = adapter();
        claim_owner(&admin, None).await.unwrap();
        issue_token(&admin, Scope::Agent, Some("Main".to_owned()), "2026-09-30")
            .await
            .unwrap();
        let owner = sole_owner_or_refuse(&admin).await.unwrap();
        let tokens = admin.list_tokens(owner).await.unwrap();
        assert!(
            tokens.iter().any(|token| token.label == "Main"),
            "{tokens:?}"
        );
    }

    // --- The listing --------------------------------------------------------

    #[tokio::test]
    async fn listing_orders_active_first_then_revoked_and_shows_no_secret() {
        let admin = adapter();
        claim_owner(&admin, None).await.unwrap();
        let agent_token = issue_token(
            &admin,
            Scope::Agent,
            Some("home agent".to_owned()),
            "2026-09-30",
        )
        .await
        .unwrap();
        issue_token(&admin, Scope::ReadOnly, None, "2026-09-30")
            .await
            .unwrap();
        revoke_token(&admin, "home agent").await.unwrap();

        let listing = list_tokens(&admin).await.unwrap();
        let lines: Vec<&str> = listing.lines().collect();
        assert_eq!(lines.len(), 3, "{listing}");

        assert!(lines[0].contains(" owner "), "{listing}");
        assert!(lines[0].contains("active"), "{listing}");
        assert!(lines[1].contains("read-only 2026-09-30"), "{listing}");
        assert!(lines[1].contains("created "), "{listing}");
        assert!(lines[1].contains("active"), "{listing}");
        assert!(lines[2].contains("home agent"), "{listing}");
        assert!(
            lines[2].contains("revoked "),
            "the revoked token names when it stopped working: {listing}"
        );

        assert!(!listing.contains(&agent_token), "{listing}");
        assert!(
            !listing.contains(&iaam_app::tokens::hash_token(&agent_token)),
            "{listing}"
        );
    }

    #[tokio::test]
    async fn the_listing_of_an_empty_instance_is_refused_not_blank() {
        let admin = adapter();
        let error = list_tokens(&admin).await.unwrap_err();
        assert!(error.to_string().contains("iaam claim"), "{error}");
    }

    // --- Revocation ---------------------------------------------------------

    #[tokio::test]
    async fn revocation_takes_effect_at_once_through_the_authentication_seam() {
        let admin = adapter();
        claim_owner(&admin, None).await.unwrap();
        let agent_token = issue_token(
            &admin,
            Scope::Agent,
            Some("home agent".to_owned()),
            "2026-09-30",
        )
        .await
        .unwrap();
        let hash = iaam_app::tokens::hash_token(&agent_token);

        let principal = Store::find_principal(&admin, hash.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(principal.scope, Scope::Agent);

        let report = revoke_token(&admin, "home agent").await.unwrap();
        assert!(report.contains("home agent"), "{report}");
        assert!(report.contains("agent"), "{report}");

        let after = Store::find_principal(&admin, hash).await.unwrap();
        assert!(after.is_none(), "a revoked token is refused at once");
    }

    #[tokio::test]
    async fn an_ambiguous_label_is_refused_with_the_ids_to_revoke_by() {
        let admin = adapter();
        claim_owner(&admin, None).await.unwrap();
        issue_token(&admin, Scope::Agent, Some("dup".to_owned()), "2026-09-30")
            .await
            .unwrap();
        issue_token(&admin, Scope::Agent, Some("dup".to_owned()), "2026-10-01")
            .await
            .unwrap();

        let error = revoke_token(&admin, "dup").await.unwrap_err();
        let text = error.to_string();
        assert!(text.contains("dup"), "{text}");

        let owner = sole_owner_or_refuse(&admin).await.unwrap();
        let tokens = admin.list_tokens(owner).await.unwrap();
        let dup_ids: Vec<String> = tokens
            .iter()
            .filter(|token| token.label == "dup")
            .map(|token| token.id.to_string())
            .collect();
        assert_eq!(dup_ids.len(), 2, "{text}");
        let unambiguous_ids: Vec<String> = tokens
            .iter()
            .filter(|token| token.label != "dup")
            .map(|token| token.id.to_string())
            .collect();
        assert_eq!(unambiguous_ids.len(), 1, "{text}");
        for id in &dup_ids {
            assert!(text.contains(id.as_str()), "{text}");
        }
        assert!(
            !text.contains(unambiguous_ids[0].as_str()),
            "the refusal names only the ambiguous tokens: {text}"
        );

        // The refusal leaves both alone; the owner revokes one by id.
        revoke_token(&admin, &dup_ids[0]).await.unwrap();
        let listing = list_tokens(&admin).await.unwrap();
        let revoked = listing
            .lines()
            .filter(|line| line.contains("dup"))
            .filter(|line| line.contains("revoked "))
            .count();
        assert_eq!(revoked, 1, "{listing}");
    }

    #[tokio::test]
    async fn an_unknown_label_or_id_is_refused_naming_what_was_looked_for() {
        let admin = adapter();
        claim_owner(&admin, None).await.unwrap();

        let error = revoke_token(&admin, "ghost").await.unwrap_err();
        assert!(error.to_string().contains("ghost"), "{error}");

        let absent = uuid::Uuid::new_v4().to_string();
        let error = revoke_token(&admin, &absent).await.unwrap_err();
        assert!(error.to_string().contains(&absent), "{error}");
    }

    #[tokio::test]
    async fn revoking_an_already_revoked_token_is_refused() {
        let admin = adapter();
        claim_owner(&admin, None).await.unwrap();
        issue_token(&admin, Scope::Agent, Some("once".to_owned()), "2026-09-30")
            .await
            .unwrap();
        revoke_token(&admin, "once").await.unwrap();

        let error = revoke_token(&admin, "once").await.unwrap_err();
        assert!(error.to_string().contains("already revoked"), "{error}");
    }

    #[tokio::test]
    async fn revoking_on_an_empty_instance_names_the_claim_command() {
        let admin = adapter();
        let error = revoke_token(&admin, "ghost").await.unwrap_err();
        assert!(error.to_string().contains("iaam claim"), "{error}");
    }

    #[tokio::test]
    async fn issuing_on_an_empty_instance_names_the_claim_command() {
        let admin = adapter();
        let error = issue_token(&admin, Scope::Owner, None, "2026-09-30")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("iaam claim"), "{error}");
    }

    #[tokio::test]
    async fn a_label_with_a_control_character_is_refused_naming_it() {
        let admin = adapter();
        claim_owner(&admin, None).await.unwrap();

        let error = issue_token(
            &admin,
            Scope::Agent,
            Some("agent\n2".to_owned()),
            "2026-09-30",
        )
        .await
        .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("invalid"), "{text}");
        assert!(text.contains("U+000A"), "{text}");
        assert!(text.contains("label"), "{text}");

        // The refusal happened before the port was called: nothing issued.
        let owner = sole_owner_or_refuse(&admin).await.unwrap();
        assert_eq!(
            admin.list_tokens(owner).await.unwrap().len(),
            1,
            "the refused label issued nothing"
        );
    }

    #[tokio::test]
    async fn a_claim_label_with_a_control_character_is_refused_before_the_claim() {
        let admin = adapter();

        let error = claim_owner(&admin, Some("owner\u{1}".to_owned()))
            .await
            .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("U+0001"), "{text}");

        // The instance is still unclaimed: the bad label claimed nothing.
        let error = sole_owner_or_refuse(&admin).await.unwrap_err();
        assert!(error.to_string().contains("iaam claim"), "{error}");
    }

    #[tokio::test]
    async fn the_listing_renders_stored_control_characters_harmlessly() {
        let admin = adapter();
        claim_owner(&admin, None).await.unwrap();
        // A label issued over HTTP before the refusal existed: the port
        // takes it, so the list must render it safely.
        let owner = sole_owner_or_refuse(&admin).await.unwrap();
        admin
            .issue_token(owner, "a\u{1}b".to_owned(), Scope::Agent)
            .await
            .unwrap();

        let listing = list_tokens(&admin).await.unwrap();
        let lines: Vec<&str> = listing.lines().collect();
        assert_eq!(lines.len(), 2, "{listing}");
        assert!(listing.contains("a\\u{1}b"), "{listing}");
        assert!(!listing.contains('\u{1}'), "{listing}");
    }

    #[tokio::test]
    async fn a_uuid_shaped_label_is_revoked_by_label() {
        let admin = adapter();
        claim_owner(&admin, None).await.unwrap();
        let label = uuid::Uuid::new_v4().to_string();
        issue_token(&admin, Scope::Agent, Some(label.clone()), "2026-09-30")
            .await
            .unwrap();

        let report = revoke_token(&admin, &label).await.unwrap();
        assert!(report.contains("agent"), "{report}");
        assert!(report.contains(&label), "{report}");

        // The token is revoked: revoking the same label again says so.
        let error = revoke_token(&admin, &label).await.unwrap_err();
        assert!(error.to_string().contains("already revoked"), "{error}");
    }

    #[tokio::test]
    async fn a_uuid_shaped_label_that_is_ambiguous_still_refuses_with_ids() {
        let admin = adapter();
        claim_owner(&admin, None).await.unwrap();
        let label = uuid::Uuid::new_v4().to_string();
        issue_token(&admin, Scope::Agent, Some(label.clone()), "2026-09-30")
            .await
            .unwrap();
        issue_token(&admin, Scope::ReadOnly, Some(label.clone()), "2026-09-30")
            .await
            .unwrap();

        let error = revoke_token(&admin, &label).await.unwrap_err();
        let text = error.to_string();
        assert!(text.contains("active tokens"), "{text}");

        let owner = sole_owner_or_refuse(&admin).await.unwrap();
        for token in admin.list_tokens(owner).await.unwrap() {
            if token.label == label {
                assert!(text.contains(&token.id.to_string()), "{text}");
            }
        }
    }
}
