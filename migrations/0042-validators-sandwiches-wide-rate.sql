-- NULL on rows stored before this migration and on epochs published without the column.
ALTER TABLE validators_sandwiches ADD COLUMN wide_sandwich_rate_30d DOUBLE PRECISION DEFAULT NULL;
