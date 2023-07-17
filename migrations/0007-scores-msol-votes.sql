alter table scores add column "msol_votes" numeric NOT NULL default 0;
alter table scores alter column "msol_votes" drop default;