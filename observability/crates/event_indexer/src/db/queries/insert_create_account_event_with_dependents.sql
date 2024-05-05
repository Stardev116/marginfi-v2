WITH upsert_authority AS (
    INSERT INTO users (address)
    VALUES ($1)
    ON CONFLICT (address) DO NOTHING
    RETURNING id
), existing_authority AS (
    SELECT id FROM users WHERE address = $1
), combined_authority AS (
    SELECT id FROM upsert_authority
    UNION ALL
    SELECT id FROM existing_authority
    LIMIT 1
),
upsert_account AS (
    INSERT INTO accounts (address, user_id)
    VALUES ($2, (SELECT id FROM combined_authority))
    ON CONFLICT (address) DO NOTHING
    RETURNING id
), existing_account AS (
    SELECT id FROM accounts WHERE address = $2
), combined_account AS (
    SELECT id FROM upsert_account
    UNION ALL
    SELECT id FROM existing_account
    LIMIT 1
)
INSERT INTO create_account_events (timestamp, slot, tx_sig, in_flashloan, call_stack, account_id, authority_id)
VALUES ($3, $4, $5, $6, $7, (SELECT id FROM combined_account), (SELECT id FROM combined_authority))
RETURNING id;
