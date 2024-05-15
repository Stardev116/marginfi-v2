WITH upsert_group AS (
    INSERT INTO groups (address, admin)
    VALUES ($11, $12)
    ON CONFLICT (address) DO NOTHING
    RETURNING id
), existing_group AS (
    SELECT id FROM groups WHERE address = $11
), combined_group AS (
    SELECT id FROM upsert_group
    UNION ALL
    SELECT id FROM existing_group
    LIMIT 1
),
upsert_old_authority AS (
    INSERT INTO users (address)
    VALUES ($8)
    ON CONFLICT (address) DO NOTHING
    RETURNING id
), existing_old_authority AS (
    SELECT id FROM users WHERE address = $8
), combined_old_authority AS (
    SELECT id FROM upsert_old_authority
    UNION ALL
    SELECT id FROM existing_old_authority
    LIMIT 1
),
upsert_new_authority AS (
    INSERT INTO users (address)
    VALUES ($9)
    ON CONFLICT (address) DO NOTHING
    RETURNING id
), existing_new_authority AS (
    SELECT id FROM users WHERE address = $9
), combined_new_authority AS (
    SELECT id FROM upsert_new_authority
    UNION ALL
    SELECT id FROM existing_new_authority
    LIMIT 1
),
upsert_account AS (
    INSERT INTO accounts (address, user_id)
    VALUES ($10, (SELECT id FROM combined_new_authority))
), existing_old_authority AS (
    SELECT id FROM accounts WHERE address = $10
), combined_old_authority AS (
    SELECT id FROM upsert_old_authority
    UNION ALL
    SELECT id FROM existing_old_authority
    LIMIT 1
),
INSERT INTO transfer_account_authority_events (timestamp, slot, tx_sig, in_flashloan, call_stack, outer_ix_index, inner_ix_index, account_id, old_authority_id, new_authority_id)
VALUES ($1, $2, $3, $4, $5, $6, $7, (SELECT id FROM combined_account), (SELECT id FROM combined_old_authority), (SELECT id FROM combined_new_authority))
RETURNING id;
