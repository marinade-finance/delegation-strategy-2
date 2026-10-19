-- Databases that got 0000 to 0038 by hand, before `store migrate` recorded them.
-- validators_rewards.expected_take_rate comes from 0038.
DO $adopt$
BEGIN
    IF to_regclass('validators') IS NULL THEN
        RETURN;
    END IF;
    IF NOT EXISTS (
        SELECT FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'validators_rewards'
          AND column_name = 'expected_take_rate'
    ) THEN
        RAISE EXCEPTION 'apply migrations up to 0038 by hand before store migrate can adopt this database';
    END IF;
    INSERT INTO migrations (name) VALUES
        ('0000-init.sql'),
        ('0002-scores.sql'),
        ('0003-scores-with-ranks.sql'),
        ('0004-vote-account-data.sql'),
        ('0005-scores-component-values.sql'),
        ('0006-scores-mnde-votes.sql'),
        ('0007-scores-msol-votes.sql'),
        ('0008-foundation-stake.sql'),
        ('0009-self-stake.sql'),
        ('0010-native-stake.sql'),
        ('0011-indexes.sql'),
        ('0012-institutional-stake.sql'),
        ('0013-migrations-fix.sql'),
        ('0014-jito-priority-fee.sql'),
        ('0015-validator-info-icon-url.sql'),
        ('0016-block-rewards.sql'),
        ('0017-cluster-nodes-fields.sql'),
        ('0018-events.sql'),
        ('0019-client-registry-fields.sql'),
        ('0020-drop-derived-client-columns.sql'),
        ('0021-slots-per-year.sql'),
        ('0022-cluster-info-slots-per-year.sql'),
        ('0023-node-observations.sql'),
        ('0024-ip-info.sql'),
        ('0025-validators-rewards.sql'),
        ('0026-jito-commission-indexes.sql'),
        ('0027-releases.sql'),
        ('0028-vote-state-v4-commission-collectors.sql'),
        ('0029-commission-effective-backfill.sql'),
        ('0030-commissions-epoch-index.sql'),
        ('0031-pending-stake.sql'),
        ('0032-direct-stake.sql'),
        ('0033-collector-health.sql'),
        ('0033-validators-sandwiches.sql'),
        ('0034-commission-effective-bps.sql'),
        ('0035-commission-effective-vintage-backfill.sql'),
        ('0036-validators-vote-reward-lamports.sql'),
        ('0037-validators-client-id-source.sql'),
        ('0038-validators-rewards-expected-take-rate.sql');
END;
$adopt$;
