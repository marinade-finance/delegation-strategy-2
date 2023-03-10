alter table scores add column "component_ranks" int[] NOT NULL default '{1,1,1}'::int[];
alter table scores alter column "component_ranks" drop default;
