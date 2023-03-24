ALTER TABLE warnings
ADD COLUMN vote_account TEXT NOT NULL default '';
ALTER TABLE versions
ADD COLUMN vote_account TEXT NOT NULL default '';
ALTER TABLE uptimes
ADD COLUMN vote_account TEXT NOT NULL default '';
ALTER TABLE commissions
ADD COLUMN vote_account TEXT NOT NULL default '';
ALTER TABLE validators 
DROP CONSTRAINT validators_pkey;
ALTER TABLE validators
ADD PRIMARY KEY (vote_account, epoch);