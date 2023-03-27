ALTER TABLE versions
ADD COLUMN vote_account TEXT NOT NULL default '';
UPDATE versions
SET vote_account=validator.vote_account
FROM (SELECT identity, vote_account
      FROM  validators WHERE epoch in (SELECT MAX(epoch)
FROM validators
GROUP BY identity
LIMIT 1 )) AS validator
WHERE versions.identity=validator.identity
ALTER TABLE versions ALTER COLUMN identity DROP NOT NULL;

ALTER TABLE uptimes
ADD COLUMN vote_account TEXT NOT NULL default '';
UPDATE uptimes
SET vote_account=validator.vote_account
FROM (SELECT identity, vote_account
      FROM  validators WHERE epoch in (SELECT MAX(epoch)
FROM validators
GROUP BY identity
LIMIT 1 )) AS validator
WHERE uptimes.identity=validator.identity
ALTER TABLE uptimes ALTER COLUMN identity DROP NOT NULL;

ALTER TABLE commissions
ADD COLUMN vote_account TEXT NOT NULL default '';
UPDATE commissions
SET vote_account=validator.vote_account
FROM (SELECT identity, vote_account
      FROM  validators WHERE epoch in (SELECT MAX(epoch)
FROM validators
GROUP BY identity
LIMIT 1 )) AS validator
WHERE commissions.identity=validator.identity
ALTER TABLE commissions ALTER COLUMN identity DROP NOT NULL;

ALTER TABLE validators 
DROP CONSTRAINT validators_pkey;
ALTER TABLE validators
ADD PRIMARY KEY (vote_account, epoch);