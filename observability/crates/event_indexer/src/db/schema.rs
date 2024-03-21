// @generated automatically by Diesel CLI.

diesel::table! {
    accounts (id) {
        id -> Int4,
        address -> Varchar,
        user_id -> Int4,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    banks (id) {
        id -> Int4,
        address -> Varchar,
        mint_id -> Int4,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    borrow_events (id) {
        id -> Int4,
        timestamp -> Timestamp,
        tx_sig -> Varchar,
        in_flashloan -> Bool,
        call_stack -> Varchar,
        account_id -> Int4,
        authority_id -> Int4,
        bank_id -> Int4,
        amount -> Numeric,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    create_account_events (id) {
        id -> Int4,
        timestamp -> Timestamp,
        tx_sig -> Varchar,
        in_flashloan -> Bool,
        call_stack -> Varchar,
        account_id -> Int4,
        authority_id -> Int4,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    deposit_events (id) {
        id -> Int4,
        timestamp -> Timestamp,
        tx_sig -> Varchar,
        in_flashloan -> Bool,
        call_stack -> Varchar,
        account_id -> Int4,
        authority_id -> Int4,
        bank_id -> Int4,
        amount -> Numeric,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    mints (id) {
        id -> Int4,
        address -> Varchar,
        symbol -> Varchar,
        decimals -> Int2,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    repay_events (id) {
        id -> Int4,
        timestamp -> Timestamp,
        tx_sig -> Varchar,
        in_flashloan -> Bool,
        call_stack -> Varchar,
        account_id -> Int4,
        authority_id -> Int4,
        bank_id -> Int4,
        amount -> Numeric,
        all -> Bool,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    transfer_account_authority_events (id) {
        id -> Int4,
        timestamp -> Timestamp,
        tx_sig -> Varchar,
        in_flashloan -> Bool,
        call_stack -> Varchar,
        account_id -> Int4,
        old_authority_id -> Int4,
        new_authority_id -> Int4,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    users (id) {
        id -> Int4,
        address -> Varchar,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    withdraw_emissions_events (id) {
        id -> Int4,
        timestamp -> Timestamp,
        tx_sig -> Varchar,
        in_flashloan -> Bool,
        call_stack -> Varchar,
        account_id -> Int4,
        authority_id -> Int4,
        bank_id -> Int4,
        emission_mint_id -> Int4,
        amount -> Numeric,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    withdraw_events (id) {
        id -> Int4,
        timestamp -> Timestamp,
        tx_sig -> Varchar,
        in_flashloan -> Bool,
        call_stack -> Varchar,
        account_id -> Int4,
        authority_id -> Int4,
        bank_id -> Int4,
        amount -> Numeric,
        all -> Bool,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::joinable!(accounts -> users (user_id));
diesel::joinable!(banks -> mints (mint_id));
diesel::joinable!(borrow_events -> accounts (account_id));
diesel::joinable!(borrow_events -> banks (bank_id));
diesel::joinable!(borrow_events -> users (authority_id));
diesel::joinable!(create_account_events -> accounts (account_id));
diesel::joinable!(create_account_events -> users (authority_id));
diesel::joinable!(deposit_events -> accounts (account_id));
diesel::joinable!(deposit_events -> banks (bank_id));
diesel::joinable!(deposit_events -> users (authority_id));
diesel::joinable!(repay_events -> accounts (account_id));
diesel::joinable!(repay_events -> banks (bank_id));
diesel::joinable!(repay_events -> users (authority_id));
diesel::joinable!(transfer_account_authority_events -> accounts (account_id));
diesel::joinable!(withdraw_emissions_events -> accounts (account_id));
diesel::joinable!(withdraw_emissions_events -> banks (bank_id));
diesel::joinable!(withdraw_emissions_events -> mints (emission_mint_id));
diesel::joinable!(withdraw_emissions_events -> users (authority_id));
diesel::joinable!(withdraw_events -> accounts (account_id));
diesel::joinable!(withdraw_events -> banks (bank_id));
diesel::joinable!(withdraw_events -> users (authority_id));

diesel::allow_tables_to_appear_in_same_query!(
    accounts,
    banks,
    borrow_events,
    create_account_events,
    deposit_events,
    mints,
    repay_events,
    transfer_account_authority_events,
    users,
    withdraw_emissions_events,
    withdraw_events,
);
