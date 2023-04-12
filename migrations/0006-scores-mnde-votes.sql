alter table scores add column "mnde_votes" numeric NOT NULL default 0;
alter table scores alter column "mnde_votes" drop default;