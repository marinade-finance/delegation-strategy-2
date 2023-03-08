CREATE TABLE "scoring_runs" (
    "scoring_run_id" BIGSERIAL NOT NULL,
    "created_at" timestamp with time zone NOT NULL,
    "epoch" int NOT NULL,
    "components" text[] NOT NULL,
    "component_weights" double precision[] NOT NULL,
    "ui_id" text NOT NULL,
    PRIMARY KEY("scoring_run_id")
);
CREATE TABLE "scores" (
    "score_id" bigserial,
    "vote_account" text NOT NULL,
    "score" double precision NOT NULL,
    "component_scores" double precision[] NOT NULL,
    "rank" int NOT NULL,
    "ui_hints" text[] NOT NULL,
    "eligible_stake_algo" boolean,
    "eligible_stake_mnde" boolean,
    "eligible_stake_msol" boolean,
    "target_stake_algo" numeric,
    "target_stake_mnde" numeric,
    "target_stake_msol" numeric,
    "scoring_run_id" bigint,
    PRIMARY KEY ("score_id"),
    FOREIGN KEY ("scoring_run_id") REFERENCES "scoring_runs"("scoring_run_id")
);