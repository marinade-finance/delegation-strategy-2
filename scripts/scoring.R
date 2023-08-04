library(dotenv)
library(semver)
library(data.table)

normalize <- function(x, na.rm = TRUE) {
  return ((x - min(x, na.rm = TRUE)) / (max(x, na.rm = TRUE) - min(x, na.rm = TRUE)))
}

if (length(commandArgs(trailingOnly=TRUE)) > 0) {
  args <- commandArgs(trailingOnly=TRUE)
}
file_out_scores <- args[1]
file_out_stakes <- args[2]
file_params <- args[3]
file_blacklist <- args[4]
file_validators <- args[5]
file_msol_votes <- args[6]
file_vemnde_votes <- args[7]

t(data.frame(
  file_out_scores,
  file_out_stakes,
  file_params,
  file_blacklist,
  file_validators,
  file_msol_votes,
  file_vemnde_votes
))

vemnde_votes <- read.csv(file_vemnde_votes)
msol_votes <- read.csv(file_msol_votes)
validators <- read.csv(file_validators)
blacklist <- read.csv(file_blacklist)
load_dot_env(file = file_params)

TOTAL_STAKE=as.numeric(Sys.getenv("TOTAL_STAKE"))

MARINADE_VALIDATORS_COUNT <- as.numeric(Sys.getenv("MARINADE_VALIDATORS_COUNT"))

WEIGHT_ADJUSTED_CREDITS <- as.numeric(Sys.getenv("WEIGHT_ADJUSTED_CREDITS"))
WEIGHT_GRACE_SKIP_RATE <- as.numeric(Sys.getenv("WEIGHT_GRACE_SKIP_RATE"))
WEIGHT_DC_CONCENTRATION <- as.numeric(Sys.getenv("WEIGHT_DC_CONCENTRATION"))

ELIGIBILITY_ALGO_STAKE_MAX_COMMISSION <- as.numeric(Sys.getenv("ELIGIBILITY_ALGO_STAKE_MAX_COMMISSION"))
ELIGIBILITY_ALGO_STAKE_MIN_STAKE <- as.numeric(Sys.getenv("ELIGIBILITY_ALGO_STAKE_MIN_STAKE"))

ELIGIBILITY_VEMNDE_STAKE_MAX_COMMISSION <- as.numeric(Sys.getenv("ELIGIBILITY_VEMNDE_STAKE_MAX_COMMISSION"))
ELIGIBILITY_VEMNDE_STAKE_MIN_STAKE <- as.numeric(Sys.getenv("ELIGIBILITY_VEMNDE_STAKE_MIN_STAKE"))
ELIGIBILITY_VEMNDE_SCORE_THRESHOLD_MULTIPLIER <- as.numeric(Sys.getenv("ELIGIBILITY_VEMNDE_SCORE_THRESHOLD_MULTIPLIER"))

ELIGIBILITY_MSOL_STAKE_MAX_COMMISSION <- as.numeric(Sys.getenv("ELIGIBILITY_MSOL_STAKE_MAX_COMMISSION"))
ELIGIBILITY_MSOL_STAKE_MIN_STAKE <- as.numeric(Sys.getenv("ELIGIBILITY_MSOL_STAKE_MIN_STAKE"))
ELIGIBILITY_MSOL_SCORE_THRESHOLD_MULTIPLIER <- as.numeric(Sys.getenv("ELIGIBILITY_MSOL_SCORE_THRESHOLD_MULTIPLIER"))

ELIGIBILITY_MIN_VERSION <- Sys.getenv("ELIGIBILITY_MIN_VERSION")

VEMNDE_VALIDATOR_CAP <- as.numeric(Sys.getenv("VEMNDE_VALIDATOR_CAP"))

STAKE_CONTROL_VEMNDE <- as.numeric(Sys.getenv("STAKE_CONTROL_VEMNDE"))
STAKE_CONTROL_MSOL <- as.numeric(Sys.getenv("STAKE_CONTROL_MSOL"))
STAKE_CONTROL_ALGO <- 1 - STAKE_CONTROL_VEMNDE - STAKE_CONTROL_MSOL

# Perform min-max normalization of algo staking formula's components
validators$normalized_dc_concentration <- normalize(1 - validators$avg_dc_concentration)
validators$normalized_grace_skip_rate <- normalize(1 - validators$avg_grace_skip_rate)
validators$normalized_adjusted_credits <- normalize(validators$avg_adjusted_credits)
validators$rank_dc_concentration <- rank(-validators$normalized_dc_concentration, ties.method="min")
validators$rank_grace_skip_rate <- rank(-validators$normalized_grace_skip_rate, ties.method="min")
validators$rank_adjusted_credits <- rank(-validators$normalized_adjusted_credits, ties.method="min")

# Apply the algo staking formula on all validators
validators$score <- (0
                     + validators$normalized_dc_concentration * WEIGHT_DC_CONCENTRATION
                     + validators$normalized_grace_skip_rate * WEIGHT_GRACE_SKIP_RATE
                     + validators$normalized_adjusted_credits * WEIGHT_ADJUSTED_CREDITS
) / (WEIGHT_ADJUSTED_CREDITS + WEIGHT_GRACE_SKIP_RATE + WEIGHT_DC_CONCENTRATION)

# Apply blacklist
validators$blacklisted <- 0
for (i in 1:nrow(validators)) {
  blacklist_reasons <- blacklist[blacklist$vote_account == validators[i, "vote_account"],]
  if (nrow(blacklist_reasons) > 0) {
    for (j in 1:nrow(blacklist_reasons)) {
        validators[i, "blacklisted"] <- 1
        validators[i, "ui_hints"][[1]] <- list(c(validators[i, "ui_hints"][[1]], blacklist_reasons[j, "code"]))
    }
  }
}

# Apply algo staking eligibility criteria
validators$eligible_stake_algo <- 1 - validators$blacklisted
validators$eligible_stake_algo[validators$max_commission > ELIGIBILITY_ALGO_STAKE_MAX_COMMISSION] <- 0
validators$eligible_stake_algo[validators$minimum_stake < ELIGIBILITY_ALGO_STAKE_MIN_STAKE] <- 0
validators$eligible_stake_algo[parse_version(validators$version) < ELIGIBILITY_MIN_VERSION] <- 0

validators$eligible_stake_msol <- validators$eligible_stake_algo

for (i in 1:nrow(validators)) {
  if (validators[i, "max_commission"] > ELIGIBILITY_ALGO_STAKE_MAX_COMMISSION) {
    validators[i, "ui_hints"][[1]] <- list(c(validators[i, "ui_hints"][[1]], "NOT_ELIGIBLE_ALGO_STAKE_MAX_COMMISSION_OVER_10"))
  }
  if (validators[i, "minimum_stake"] < ELIGIBILITY_ALGO_STAKE_MIN_STAKE) {
    validators[i, "ui_hints"][[1]] <- list(c(validators[i, "ui_hints"][[1]], "NOT_ELIGIBLE_ALGO_STAKE_MIN_STAKE_BELOW_1000"))
  }
  if (parse_version(validators[i, "version"]) < ELIGIBILITY_MIN_VERSION) {
    validators[i, "ui_hints"][[1]] <- list(c(validators[i, "ui_hints"][[1]], "NOT_ELIGIBLE_VERSION_TOO_LOW"))
  }
}

# Sort validators to find the eligible validators with the best score
validators$rank <- rank(-validators$score, ties.method="min")
validators <- validators[order(validators$rank),]
validators_algo_set <- head(validators[validators$eligible_stake_algo == 1,], MARINADE_VALIDATORS_COUNT)
min_score_in_algo_set <- min(validators_algo_set$score)

# Mark validator who should receive algo stake
validators$in_algo_stake_set <- 0
validators$in_algo_stake_set[validators$score >= min_score_in_algo_set] <- 1
validators$in_algo_stake_set[validators$eligible_stake_algo == 0] <- 0

# Mark msol votes for each validator
validators$msol_votes <- 0
if (nrow(msol_votes) > 0) {
  for (i in 1:nrow(msol_votes)) {
    validators[validators$vote_account == msol_votes[i, "vote_account"], ]$msol_votes <- msol_votes[i, "msol_votes"]
  }
}

# Mark veMNDE votes for each validator
validators$vemnde_votes <- 0
if (nrow(vemnde_votes) > 0) {
  for (i in 1:nrow(vemnde_votes)) {
    matching_rows <- validators$vote_account == vemnde_votes[i, "vote_account"]
    if (any(matching_rows)) {
      validators[matching_rows, "vemnde_votes"] <- vemnde_votes[i, "vemnde_votes"]
    }
  }
}

# Convert from lamports
validators$vemnde_votes <- validators$vemnde_votes

# Apply msol staking eligibility criteria
validators$eligible_stake_msol <- 1 - validators$blacklisted
validators$eligible_stake_msol[validators$max_commission > ELIGIBILITY_MSOL_STAKE_MAX_COMMISSION] <- 0
validators$eligible_stake_msol[validators$minimum_stake < ELIGIBILITY_MSOL_STAKE_MIN_STAKE] <- 0
validators$eligible_stake_msol[validators$score < min_score_in_algo_set * ELIGIBILITY_MSOL_SCORE_THRESHOLD_MULTIPLIER] <- 0
validators$eligible_stake_msol[parse_version(validators$version) < ELIGIBILITY_MIN_VERSION] <- 0 # UI hint provided earlier

for (i in 1:nrow(validators)) {
  if (validators[i, "max_commission"] > ELIGIBILITY_MSOL_STAKE_MAX_COMMISSION) {
    validators[i, "ui_hints"][[1]] <- list(c(validators[i, "ui_hints"][[1]], "NOT_ELIGIBLE_MSOL_STAKE_MAX_COMMISSION_OVER_10"))
  }
  if (validators[i, "minimum_stake"] < ELIGIBILITY_MSOL_STAKE_MIN_STAKE) {
    validators[i, "ui_hints"][[1]] <- list(c(validators[i, "ui_hints"][[1]], "NOT_ELIGIBLE_MSOL_STAKE_MIN_STAKE_BELOW_100"))
  }
  if (validators[i, "score"] < min_score_in_algo_set * ELIGIBILITY_MSOL_SCORE_THRESHOLD_MULTIPLIER) {
    validators[i, "ui_hints"][[1]] <- list(c(validators[i, "ui_hints"][[1]], "NOT_ELIGIBLE_MSOL_STAKE_SCORE_TOO_LOW"))
  }
}

# Apply eligibility on votes to get effective votes
msol_valid_votes <- round(validators$msol_votes * validators$eligible_stake_msol)
msol_valid_votes_total <- sum(msol_valid_votes)

validators$msol_power <- 0
if (msol_valid_votes_total > 0) {
  validators$msol_power <- msol_valid_votes / msol_valid_votes_total
}

# Apply VeMNDE staking eligibility criteria
validators$eligible_stake_vemnde <- 1 - validators$blacklisted
validators$eligible_stake_vemnde[validators$max_commission > ELIGIBILITY_VEMNDE_STAKE_MAX_COMMISSION] <- 0
validators$eligible_stake_vemnde[validators$minimum_stake < ELIGIBILITY_VEMNDE_STAKE_MIN_STAKE] <- 0
validators$eligible_stake_vemnde[validators$score < min_score_in_algo_set * ELIGIBILITY_VEMNDE_SCORE_THRESHOLD_MULTIPLIER] <- 0
validators$eligible_stake_vemnde[parse_version(validators$version) < ELIGIBILITY_MIN_VERSION] <- 0 # UI hint provided earlier

for (i in 1:nrow(validators)) {
  if (validators[i, "max_commission"] > ELIGIBILITY_VEMNDE_STAKE_MAX_COMMISSION) {
    validators[i, "ui_hints"][[1]] <- list(c(validators[i, "ui_hints"][[1]], "NOT_ELIGIBLE_VEMNDE_STAKE_MAX_COMMISSION_OVER_10"))
  }
  if (validators[i, "minimum_stake"] < ELIGIBILITY_VEMNDE_STAKE_MIN_STAKE) {
    validators[i, "ui_hints"][[1]] <- list(c(validators[i, "ui_hints"][[1]], "NOT_ELIGIBLE_VEMNDE_STAKE_MIN_STAKE_BELOW_100"))
  }
  if (validators[i, "score"] < min_score_in_algo_set * ELIGIBILITY_VEMNDE_SCORE_THRESHOLD_MULTIPLIER) {
    validators[i, "ui_hints"][[1]] <- list(c(validators[i, "ui_hints"][[1]], "NOT_ELIGIBLE_VEMNDE_STAKE_SCORE_TOO_LOW"))
  }
}

# Apply eligibility on votes to get effective votes
vemnde_valid_votes <- round(validators$vemnde_votes * validators$eligible_stake_vemnde)
vemnde_valid_votes_total <- sum(vemnde_valid_votes)

# Apply cap on the share of vemnde votes
vemnde_power_cap <- round(sum(vemnde_valid_votes) * VEMNDE_VALIDATOR_CAP)
validators$vemnde_power <- pmin(vemnde_valid_votes, vemnde_power_cap)

# Find out how much votes got truncated
vemnde_overflow <- sum(vemnde_valid_votes) - sum(validators$vemnde_power)

# Sort validators by veMNDE power
validators <- validators[order(validators$vemnde_power, decreasing = T),]

# Distribute the overflow from the capping
for (v in 1:length(validators$vemnde_power)) {
  validators_index <- seq(1, along.with = validators$vemnde_power)
  # Ignore weights of already processed validators as they 1) already received their share from the overflow; 2) were overflowing
  moving_weights <- (validators_index > v - 1) * validators$vemnde_power
  # Break the loop if no one else should receive stake from the vemnde voting
  if (sum(moving_weights) == 0) {
    break
  }
  # How much should the power increase from the overflow
  vemnde_power_increase <- round(vemnde_overflow * moving_weights[v] / sum(moving_weights))
  # Limit the increase of vemnde power if cap should be applied
  vemnde_power_increase_capped <- min(vemnde_power_increase, vemnde_power_cap - moving_weights[v])
  # Increase vemnde power for this validator
  validators$vemnde_power <- validators$vemnde_power + (validators_index == v) * vemnde_power_increase_capped
  # Reduce the overflow by what was given to this validator
  vemnde_overflow <- vemnde_overflow - vemnde_power_increase_capped
}

# Scale vemnde power to a percentage
if (sum(validators$vemnde_power) > 0) {
  total_vemnde_power <- sum(validators$vemnde_power, vemnde_overflow)
  validators$vemnde_power <- validators$vemnde_power / total_vemnde_power
  vemnde_overflow_power <- vemnde_overflow / total_vemnde_power
} else {
  vemnde_overflow_power <- 1
}

STAKE_CONTROL_VEMNDE_SOL <- TOTAL_STAKE * STAKE_CONTROL_VEMNDE * (1 - vemnde_overflow_power)
STAKE_CONTROL_VEMNDE_OVERFLOW_SOL <- vemnde_overflow_power * TOTAL_STAKE * STAKE_CONTROL_VEMNDE
STAKE_CONTROL_MSOL_SOL <- if (msol_valid_votes_total > 0) { TOTAL_STAKE * STAKE_CONTROL_MSOL } else { 0 }
STAKE_CONTROL_MSOL_UNUSED_SOL <- if (msol_valid_votes_total > 0) { 0 } else { TOTAL_STAKE * STAKE_CONTROL_MSOL }
STAKE_CONTROL_ALGO_SOL <- TOTAL_STAKE * STAKE_CONTROL_ALGO + STAKE_CONTROL_VEMNDE_OVERFLOW_SOL + STAKE_CONTROL_MSOL_UNUSED_SOL

validators$target_stake_vemnde <- round(validators$vemnde_power * STAKE_CONTROL_VEMNDE_SOL)
validators$target_stake_msol <- round(validators$msol_power * STAKE_CONTROL_MSOL_SOL)
validators$target_stake_algo <- round(validators$score * validators$in_algo_stake_set / sum(validators$score * validators$in_algo_stake_set) * STAKE_CONTROL_ALGO_SOL)
validators$target_stake <- validators$target_stake_vemnde + validators$target_stake_algo + validators$target_stake_msol

perf_target_stake_vemnde <- sum(validators$avg_adjusted_credits * validators$target_stake_vemnde) / sum(validators$target_stake_vemnde)
perf_target_stake_algo <- sum(validators$avg_adjusted_credits * validators$target_stake_algo) / sum(validators$target_stake_algo)
perf_target_stake_msol <- sum(validators$avg_adjusted_credits * validators$target_stake_msol) / sum(validators$target_stake_msol)

print(t(data.frame(
  TOTAL_STAKE,
  STAKE_CONTROL_MSOL_SOL,
  STAKE_CONTROL_MSOL_UNUSED_SOL,
  STAKE_CONTROL_VEMNDE_SOL,
  STAKE_CONTROL_VEMNDE_OVERFLOW_SOL,
  STAKE_CONTROL_ALGO_SOL,
  perf_target_stake_vemnde,
  perf_target_stake_algo,
  perf_target_stake_msol
)))

stopifnot(TOTAL_STAKE > 3e6)
stopifnot(STAKE_CONTROL_MSOL_SOL > 900000)
stopifnot(nrow(validators) > 1000)
stopifnot(nrow(validators[validators$target_stake_algo > 0,]) == 100)

validators$ui_hints <- lapply(validators$ui_hints, paste, collapse = ',')

fwrite(validators[order(validators$rank),], file = file_out_scores, scipen = 1000, quote = T)
stakes <- validators[validators$target_stake > 0,]
fwrite(stakes[order(stakes$target_stake, decreasing = T),], file = file_out_stakes, scipen = 1000)
