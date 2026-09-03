//! The identity a source prints for an account, its aliases, and its class
//! (decision 0004).

use iaam_core::ids::{AccountId, OwnerId};
use iaam_core::instrument::AliasInterval;
use iaam_core::report::balances::NegativeBalanceExpectation;
use iaam_store::reference::{
    AccountAliasRecord, AccountCreation, AccountDeclarations, AccountDetailRecord, AccountIdentity,
    AccountRecord, CashAssetClass, Declared,
};
use iaam_store::{SqliteStore, StoreError};
use time::macros::date;

fn plain(owner: OwnerId, title: &str) -> AccountDetailRecord {
    AccountDetailRecord {
        id: AccountId::new_random(),
        owner,
        title: title.into(),
        institution: None,
        identity: None,
        cash_class: None,
        negative_balance_expectation: None,
        aliases: Vec::new(),
    }
}

#[test]
fn an_account_created_without_an_identity_carries_none() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let record = plain(owner, "Main");

    let created = store.create_account(&record).unwrap();

    assert_eq!(created, AccountCreation::Created(record.clone()));
    assert_eq!(store.list_account_details(owner).unwrap(), vec![record]);
}

#[test]
fn two_accounts_without_an_identity_are_never_the_same_account() {
    // The uniqueness constraint tolerates absence, and absence is not a value
    // two accounts can share.
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();

    store.create_account(&plain(owner, "Main")).unwrap();
    store.create_account(&plain(owner, "Savings")).unwrap();

    assert_eq!(store.list_account_details(owner).unwrap().len(), 2);
}

#[test]
fn a_create_repeating_an_identity_returns_the_account_created_last_time() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let identity = AccountIdentity {
        provider: "bank-one".into(),
        provider_account_id: "opaque-1".into(),
    };
    let first = AccountDetailRecord {
        identity: Some(identity.clone()),
        ..plain(owner, "Main")
    };
    let AccountCreation::Created(stored) = store.create_account(&first).unwrap() else {
        panic!("the first create mints an account");
    };

    // A second create carrying the same identity but a different title. The
    // title is a display name; it does not re-identify anything and it does not
    // rename what the owner already has.
    let second = AccountDetailRecord {
        identity: Some(identity),
        ..plain(owner, "Main (renamed at the source)")
    };
    let repeated = store.create_account(&second).unwrap();

    assert_eq!(repeated, AccountCreation::Existing(stored.clone()));
    assert_eq!(store.list_account_details(owner).unwrap(), vec![stored]);
}

#[test]
fn one_identity_belongs_to_one_owner_each() {
    // `(owner, provider, provider_account_id)` is the key: the same printed
    // identifier at the same provider is a different account for a different
    // owner, and reading one owner's identity into another's account would be
    // reading someone else's money (§14).
    let mut store = SqliteStore::open_in_memory().unwrap();
    let one = OwnerId::new_random();
    let another = OwnerId::new_random();
    let identity = AccountIdentity {
        provider: "bank-one".into(),
        provider_account_id: "opaque-1".into(),
    };

    store
        .create_account(&AccountDetailRecord {
            identity: Some(identity.clone()),
            ..plain(one, "Main")
        })
        .unwrap();
    let other = store
        .create_account(&AccountDetailRecord {
            identity: Some(identity),
            ..plain(another, "Main")
        })
        .unwrap();

    assert!(matches!(other, AccountCreation::Created(_)));
    assert_eq!(store.list_account_details(one).unwrap().len(), 1);
    assert_eq!(store.list_account_details(another).unwrap().len(), 1);
}

#[test]
fn one_identity_at_two_providers_is_two_accounts() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();

    store
        .create_account(&AccountDetailRecord {
            identity: Some(AccountIdentity {
                provider: "bank-one".into(),
                provider_account_id: "7".into(),
            }),
            ..plain(owner, "Main")
        })
        .unwrap();
    store
        .create_account(&AccountDetailRecord {
            identity: Some(AccountIdentity {
                provider: "bank-two".into(),
                provider_account_id: "7".into(),
            }),
            ..plain(owner, "Savings")
        })
        .unwrap();

    assert_eq!(store.list_account_details(owner).unwrap().len(), 2);
}

#[test]
fn two_cards_over_one_account_are_one_account_with_two_aliases() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let record = AccountDetailRecord {
        aliases: vec![
            AccountAliasRecord {
                value: "card-one".into(),
                interval: AliasInterval {
                    valid_from: date!(2024 - 01 - 01),
                    // The card stopped working: the interval closed, and that
                    // is the whole record of it.
                    valid_to: Some(date!(2025 - 03 - 01)),
                },
            },
            AccountAliasRecord {
                value: "card-two".into(),
                interval: AliasInterval {
                    valid_from: date!(2025 - 03 - 01),
                    valid_to: None,
                },
            },
        ],
        ..plain(owner, "Main")
    };

    store.create_account(&record).unwrap();

    let stored = store.list_account_details(owner).unwrap();
    assert_eq!(stored, vec![record]);
}

#[test]
fn a_cash_class_round_trips_through_its_code() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    for class in CashAssetClass::ALL {
        store
            .create_account(&AccountDetailRecord {
                cash_class: Some(class),
                ..plain(owner, class.code())
            })
            .unwrap();
    }

    let stored = store.list_account_details(owner).unwrap();
    let mut classes: Vec<CashAssetClass> = stored.iter().filter_map(|a| a.cash_class).collect();
    classes.sort_unstable();
    let mut expected = CashAssetClass::ALL.to_vec();
    expected.sort_unstable();
    assert_eq!(classes, expected);
}

#[test]
fn an_unknown_cash_class_code_is_not_guessed() {
    assert_eq!(CashAssetClass::from_code("brokerage"), None);
    assert_eq!(CashAssetClass::from_code("security_position"), None);
    for class in CashAssetClass::ALL {
        assert_eq!(CashAssetClass::from_code(class.code()), Some(class));
    }
}

#[test]
fn an_account_created_before_decision_0004_keeps_working() {
    // `upsert_account` is the pre-existing path and it neither knows nor invents
    // an identity. What it wrote must still be readable through the new one.
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let id = AccountId::new_random();
    store
        .upsert_account(&AccountRecord {
            id,
            owner,
            title: "Main".into(),
            institution: Some("Bank One".into()),
        })
        .unwrap();

    assert_eq!(
        store.list_account_details(owner).unwrap(),
        vec![AccountDetailRecord {
            id,
            owner,
            title: "Main".into(),
            institution: Some("Bank One".into()),
            identity: None,
            cash_class: None,
            negative_balance_expectation: None,
            aliases: Vec::new(),
        }]
    );
}

#[test]
fn a_card_that_stopped_working_is_an_alias_whose_interval_closed() {
    // The whole of the card lifecycle this model records: one interval closes,
    // another opens. Replacing the set rather than editing one row is the same
    // shape the transfer statement uses, and for the same reason — the owner
    // states what is true now, not a diff against what he said before.
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let record = AccountDetailRecord {
        aliases: vec![AccountAliasRecord {
            value: "card-one".into(),
            interval: AliasInterval {
                valid_from: date!(2024 - 01 - 01),
                valid_to: None,
            },
        }],
        ..plain(owner, "Main")
    };
    store.create_account(&record).unwrap();

    let replaced = vec![
        AccountAliasRecord {
            value: "card-one".into(),
            interval: AliasInterval {
                valid_from: date!(2024 - 01 - 01),
                valid_to: Some(date!(2025 - 03 - 01)),
            },
        },
        AccountAliasRecord {
            value: "card-two".into(),
            interval: AliasInterval {
                valid_from: date!(2025 - 03 - 01),
                valid_to: None,
            },
        },
    ];
    store
        .replace_account_aliases(owner, record.id, &replaced)
        .unwrap();

    let stored = store.list_account_details(owner).unwrap();
    assert_eq!(stored[0].aliases, replaced);
}

#[test]
fn aliases_are_not_replaced_on_another_owners_account() {
    // An identifier is not an access right: an alias written against someone
    // else's account is a statement about someone else's money (§14).
    let mut store = SqliteStore::open_in_memory().unwrap();
    let one = OwnerId::new_random();
    let another = OwnerId::new_random();
    let theirs = plain(another, "Main");
    store.create_account(&theirs).unwrap();

    let attempt = store.replace_account_aliases(
        one,
        theirs.id,
        &[AccountAliasRecord {
            value: "card-one".into(),
            interval: AliasInterval {
                valid_from: date!(2024 - 01 - 01),
                valid_to: None,
            },
        }],
    );

    assert!(attempt.is_err());
    assert!(
        store.list_account_details(another).unwrap()[0]
            .aliases
            .is_empty()
    );
}

/// The owner's expectation about a negative balance is stored and read back as
/// he stated it, and an account he said nothing about reads back as having said
/// nothing.
#[test]
fn a_negative_balance_expectation_round_trips_and_absence_stays_absence() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();

    for expectation in NegativeBalanceExpectation::ALL {
        store
            .create_account(&AccountDetailRecord {
                negative_balance_expectation: Some(expectation),
                ..plain(owner, expectation.code())
            })
            .unwrap();
    }
    store.create_account(&plain(owner, "Silent")).unwrap();

    let stored = store.list_account_details(owner).unwrap();
    let silent = stored.iter().find(|a| a.title == "Silent").unwrap();
    assert_eq!(
        silent.negative_balance_expectation, None,
        "silence is not a statement"
    );
    let mut stated: Vec<NegativeBalanceExpectation> = stored
        .iter()
        .filter_map(|a| a.negative_balance_expectation)
        .collect();
    stated.sort_unstable();
    let mut expected = NegativeBalanceExpectation::ALL.to_vec();
    expected.sort_unstable();
    assert_eq!(stated, expected);
}

/// The expectation and the class are two independent declarations on one
/// account. Neither is read to produce the other, and stating one says nothing
/// about the other — which is what decision 0004 §3 forbids deriving.
#[test]
fn the_expectation_and_the_cash_class_are_independent_declarations() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();

    // A savings account with nothing said about overdrafts: the class does not
    // imply the expectation.
    store
        .create_account(&AccountDetailRecord {
            cash_class: Some(CashAssetClass::Savings),
            ..plain(owner, "Savings, no expectation")
        })
        .unwrap();
    // A card account the owner does not expect to go negative: the expectation
    // does not follow from the class either, and the pair is his to choose.
    store
        .create_account(&AccountDetailRecord {
            cash_class: Some(CashAssetClass::CardAccount),
            negative_balance_expectation: Some(NegativeBalanceExpectation::Unexpected),
            ..plain(owner, "Card, unexpected")
        })
        .unwrap();
    // And an expectation with no class at all.
    store
        .create_account(&AccountDetailRecord {
            negative_balance_expectation: Some(NegativeBalanceExpectation::Ordinary),
            ..plain(owner, "No class, ordinary")
        })
        .unwrap();

    let stored = store.list_account_details(owner).unwrap();
    let by_title = |title: &str| {
        stored
            .iter()
            .find(|account| account.title == title)
            .unwrap()
            .clone()
    };

    let savings = by_title("Savings, no expectation");
    assert_eq!(savings.cash_class, Some(CashAssetClass::Savings));
    assert_eq!(
        savings.negative_balance_expectation, None,
        "a class never fills in an expectation"
    );

    let card = by_title("Card, unexpected");
    assert_eq!(card.cash_class, Some(CashAssetClass::CardAccount));
    assert_eq!(
        card.negative_balance_expectation,
        Some(NegativeBalanceExpectation::Unexpected)
    );

    let unclassed = by_title("No class, ordinary");
    assert_eq!(
        unclassed.cash_class, None,
        "an expectation never fills in a class"
    );
    assert_eq!(
        unclassed.negative_balance_expectation,
        Some(NegativeBalanceExpectation::Ordinary)
    );
}

/// A replacement that states nothing at all: every field is the third state,
/// and every test below overrides only the ones it is about.
fn nothing_stated() -> AccountDeclarations {
    AccountDeclarations {
        identity: Declared::Untouched,
        cash_class: Declared::Untouched,
        negative_balance_expectation: Declared::Untouched,
    }
}

#[test]
fn an_account_that_carries_no_identity_can_be_given_one() {
    // The defect: `create_account` was the only path that ever wrote these
    // three, and it ignores them when the identity is already known, because it
    // is an upsert rather than an update. So every account the owner already had
    // could never acquire an identity, a class or an expectation.
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let record = plain(owner, "Main");
    store.create_account(&record).unwrap();

    let recorded = store
        .replace_account_declarations(
            owner,
            record.id,
            &AccountDeclarations {
                identity: Declared::Stated(AccountIdentity {
                    provider: "bank-one".into(),
                    provider_account_id: "opaque-1".into(),
                }),
                cash_class: Declared::Stated(CashAssetClass::Savings),
                negative_balance_expectation: Declared::Stated(
                    NegativeBalanceExpectation::Unexpected,
                ),
            },
        )
        .unwrap();

    assert_eq!(
        recorded.account.identity,
        Some(AccountIdentity {
            provider: "bank-one".into(),
            provider_account_id: "opaque-1".into(),
        })
    );
    assert_eq!(recorded.account.cash_class, Some(CashAssetClass::Savings));
    assert_eq!(
        recorded.account.negative_balance_expectation,
        Some(NegativeBalanceExpectation::Unexpected)
    );
    assert_eq!(
        recorded.previous_identity, None,
        "a first statement displaces nothing"
    );
    assert_eq!(
        store.list_account_details(owner).unwrap(),
        vec![recorded.account]
    );
}

#[test]
fn a_declaration_the_replacement_does_not_mention_is_left_alone() {
    // The third state earning its keep: a call stating one declaration must not
    // silently withdraw the two it says nothing about.
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let record = AccountDetailRecord {
        identity: Some(AccountIdentity {
            provider: "bank-one".into(),
            provider_account_id: "opaque-1".into(),
        }),
        cash_class: Some(CashAssetClass::Savings),
        negative_balance_expectation: Some(NegativeBalanceExpectation::Unexpected),
        ..plain(owner, "Main")
    };
    store.create_account(&record).unwrap();

    let recorded = store
        .replace_account_declarations(
            owner,
            record.id,
            &AccountDeclarations {
                cash_class: Declared::Stated(CashAssetClass::Deposit),
                ..nothing_stated()
            },
        )
        .unwrap();

    assert_eq!(recorded.account.cash_class, Some(CashAssetClass::Deposit));
    assert_eq!(
        recorded.account.identity, record.identity,
        "an identity nobody mentioned is not withdrawn"
    );
    assert_eq!(
        recorded.account.negative_balance_expectation,
        Some(NegativeBalanceExpectation::Unexpected),
        "an expectation nobody mentioned is not withdrawn"
    );
    assert_eq!(recorded.previous_identity, None);
}

#[test]
fn a_declaration_stated_as_none_is_cleared() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let record = AccountDetailRecord {
        cash_class: Some(CashAssetClass::Savings),
        negative_balance_expectation: Some(NegativeBalanceExpectation::Unexpected),
        ..plain(owner, "Main")
    };
    store.create_account(&record).unwrap();

    let recorded = store
        .replace_account_declarations(
            owner,
            record.id,
            &AccountDeclarations {
                cash_class: Declared::Cleared,
                ..nothing_stated()
            },
        )
        .unwrap();

    assert_eq!(recorded.account.cash_class, None, "cleared on his word");
    assert_eq!(
        recorded.account.negative_balance_expectation,
        Some(NegativeBalanceExpectation::Unexpected),
        "and only the one he cleared"
    );
}

#[test]
fn re_pointing_an_identity_reports_the_one_it_displaced() {
    // Not refused: the journal cannot say which facts arrived under the old
    // identity, so a refusal would have to be conditioned on a different claim.
    // What the caller gets is the identity he displaced.
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let first = AccountIdentity {
        provider: "bank-one".into(),
        provider_account_id: "opaque-1".into(),
    };
    let record = AccountDetailRecord {
        identity: Some(first.clone()),
        ..plain(owner, "Main")
    };
    store.create_account(&record).unwrap();

    let second = AccountIdentity {
        provider: "bank-two".into(),
        provider_account_id: "opaque-2".into(),
    };
    let recorded = store
        .replace_account_declarations(
            owner,
            record.id,
            &AccountDeclarations {
                identity: Declared::Stated(second.clone()),
                ..nothing_stated()
            },
        )
        .unwrap();

    assert_eq!(recorded.account.identity, Some(second));
    assert_eq!(recorded.previous_identity, Some(first.clone()));

    // The displaced identity is not reserved: a create carrying it mints a new
    // account, and this account's earlier facts do not follow it there.
    let minted = store
        .create_account(&AccountDetailRecord {
            identity: Some(first),
            ..plain(owner, "Main again")
        })
        .unwrap();
    assert!(matches!(minted, AccountCreation::Created(_)));
}

#[test]
fn withdrawing_an_identity_reports_the_one_it_displaced() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let identity = AccountIdentity {
        provider: "bank-one".into(),
        provider_account_id: "opaque-1".into(),
    };
    let record = AccountDetailRecord {
        identity: Some(identity.clone()),
        ..plain(owner, "Main")
    };
    store.create_account(&record).unwrap();

    let recorded = store
        .replace_account_declarations(
            owner,
            record.id,
            &AccountDeclarations {
                identity: Declared::Cleared,
                ..nothing_stated()
            },
        )
        .unwrap();

    assert_eq!(recorded.account.identity, None);
    assert_eq!(recorded.previous_identity, Some(identity));
}

#[test]
fn restating_the_identity_an_account_already_carries_displaces_nothing() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let identity = AccountIdentity {
        provider: "bank-one".into(),
        provider_account_id: "opaque-1".into(),
    };
    let record = AccountDetailRecord {
        identity: Some(identity.clone()),
        ..plain(owner, "Main")
    };
    store.create_account(&record).unwrap();

    let recorded = store
        .replace_account_declarations(
            owner,
            record.id,
            &AccountDeclarations {
                identity: Declared::Stated(identity.clone()),
                ..nothing_stated()
            },
        )
        .unwrap();

    assert_eq!(recorded.account.identity, Some(identity));
    assert_eq!(
        recorded.previous_identity, None,
        "an account does not displace its own identity"
    );
}

#[test]
fn an_identity_another_account_already_answers_to_is_refused() {
    // Two accounts under one identity would leave the next import's upsert
    // picking between them, and the partial unique index exists to stop exactly
    // that.
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let identity = AccountIdentity {
        provider: "bank-one".into(),
        provider_account_id: "opaque-1".into(),
    };
    store
        .create_account(&AccountDetailRecord {
            identity: Some(identity.clone()),
            ..plain(owner, "Main")
        })
        .unwrap();
    let other = plain(owner, "Savings");
    store.create_account(&other).unwrap();

    let refusal = store.replace_account_declarations(
        owner,
        other.id,
        &AccountDeclarations {
            identity: Declared::Stated(identity),
            ..nothing_stated()
        },
    );

    assert!(
        matches!(refusal, Err(StoreError::AlreadyExists { .. })),
        "{refusal:?}"
    );
    let stored = store.list_account_details(owner).unwrap();
    let savings = stored
        .iter()
        .find(|account| account.id == other.id)
        .unwrap();
    assert_eq!(savings.identity, None, "the refusal wrote nothing");
}

#[test]
fn declarations_cannot_be_written_against_another_owners_account() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let one = OwnerId::new_random();
    let another = OwnerId::new_random();
    let record = plain(one, "Main");
    store.create_account(&record).unwrap();

    let refusal = store.replace_account_declarations(
        another,
        record.id,
        &AccountDeclarations {
            cash_class: Declared::Stated(CashAssetClass::Savings),
            ..nothing_stated()
        },
    );

    assert!(
        matches!(
            refusal,
            Err(StoreError::NotFound {
                what: "account",
                ..
            })
        ),
        "{refusal:?}"
    );
    assert_eq!(
        store.list_account_details(one).unwrap()[0].cash_class,
        None,
        "a statement about someone else's account records nothing"
    );
}
