library(dotenv)

normalize <- function(x, na.rm = TRUE) {
  return ((x - min(x, na.rm = TRUE)) / (max(x, na.rm = TRUE) - min(x, na.rm = TRUE)))
}

args <- commandArgs(trailingOnly = TRUE)
file_out_scores <- args[1]
file_out_stakes <- args[2]
file_params <- args[3]
file_validators <- args[4]
file_self_stake <- args[5]

t(data.frame(
  file_out_scores,
  file_out_stakes,
  file_params,
  file_validators,
  file_self_stake
))

self_stake <- read.csv(file_self_stake)
validators <- read.csv(file_validators)
load_dot_env(file = file_params)

TOTAL_STAKE=as.numeric(Sys.getenv("TOTAL_STAKE"))

MARINADE_VALIDATORS_COUNT=as.numeric(Sys.getenv("MARINADE_VALIDATORS_COUNT"))

WEIGHT_ADJUSTED_CREDITS=as.numeric(Sys.getenv("WEIGHT_ADJUSTED_CREDITS"))
WEIGHT_GRACE_SKIP_RATE=as.numeric(Sys.getenv("WEIGHT_GRACE_SKIP_RATE"))
WEIGHT_DC_CONCENTRATION=as.numeric(Sys.getenv("WEIGHT_DC_CONCENTRATION"))

ELIGIBILITY_ALGO_STAKE_MAX_COMMISSION=as.numeric(Sys.getenv("ELIGIBILITY_ALGO_STAKE_MAX_COMMISSION"))
ELIGIBILITY_ALGO_STAKE_MIN_STAKE=as.numeric(Sys.getenv("ELIGIBILITY_ALGO_STAKE_MIN_STAKE"))

ELIGIBILITY_MNDE_STAKE_MAX_COMMISSION=as.numeric(Sys.getenv("ELIGIBILITY_MNDE_STAKE_MAX_COMMISSION"))
ELIGIBILITY_MNDE_STAKE_MIN_STAKE=as.numeric(Sys.getenv("ELIGIBILITY_MNDE_STAKE_MIN_STAKE"))
ELIGIBILITY_MNDE_SCORE_THRESHOLD_MULTIPLIER=as.numeric(Sys.getenv("ELIGIBILITY_MNDE_SCORE_THRESHOLD_MULTIPLIER"))

MNDE_VALIDATOR_CAP=as.numeric(Sys.getenv("MNDE_VALIDATOR_CAP"))

STAKE_CONTROL_MNDE=as.numeric(Sys.getenv("STAKE_CONTROL_MNDE"))
STAKE_CONTROL_SELF_STAKE_MAX=as.numeric(Sys.getenv("STAKE_CONTROL_SELF_STAKE_MAX"))

# Cap self stake, so everything above x % of TVL can overflow to algo stake
self_stake$max_target_stake <- pmin(self_stake$current_balance, self_stake$deposited_balance) * (pmin(self_stake$current_balance, self_stake$deposited_balance) > 10)
self_stake_total <- sum(self_stake$max_target_stake)
self_stake_total_max <- STAKE_CONTROL_SELF_STAKE_MAX * TOTAL_STAKE
self_stake_total_capped <- min(self_stake_total, self_stake_total_max)

STAKE_CONTROL_SELF_STAKE <- self_stake_total_capped / TOTAL_STAKE
STAKE_CONTROL_SELF_STAKE_SOL <- self_stake_total_capped
STAKE_CONTROL_SELF_STAKE_OVERFLOW_SOL <- max(0, self_stake_total - self_stake_total_capped)

STAKE_CONTROL_ALGO <- 1 - STAKE_CONTROL_MNDE - STAKE_CONTROL_SELF_STAKE

# Apply self stake to validators dataframe
validators$target_stake_self <- 0
for(i in 1:nrow(self_stake)) {
  validators[validators$vote_account == self_stake[i, "vote_account"], ]$target_stake_self <- round(self_stake[i, "max_target_stake"] / sum(self_stake$max_target_stake) * self_stake_total_capped)
}

# Perform min-max normalization of algo staking formula's components
validators$normalized_dc_concentration <- normalize(1 - validators$avg_dc_concentration)
validators$normalized_grace_skip_rate <- normalize(1 - validators$avg_grace_skip_rate)
validators$normalized_adjusted_credits <- normalize(validators$avg_adjusted_credits)

# Apply the algo staking formula on all validators
validators$score <- (0
  + validators$normalized_dc_concentration * WEIGHT_DC_CONCENTRATION
  + validators$normalized_grace_skip_rate * WEIGHT_GRACE_SKIP_RATE
  + validators$normalized_adjusted_credits * WEIGHT_ADJUSTED_CREDITS
) / (WEIGHT_ADJUSTED_CREDITS + WEIGHT_GRACE_SKIP_RATE + WEIGHT_DC_CONCENTRATION)

# Apply algo staking eligibility criteria
validators$eligible_algo_stake <- TRUE
validators$eligible_algo_stake[validators$max_commission > ELIGIBILITY_ALGO_STAKE_MAX_COMMISSION] <- FALSE
validators$eligible_algo_stake[validators$minimum_stake < ELIGIBILITY_ALGO_STAKE_MIN_STAKE] <- FALSE

# Sort validators to find the eligible validators with the best score
validators$rank <- rank(-validators$score)
validators <- validators[order(validators$rank),]
validators_algo_set <- head(validators[validators$eligible_algo_stake == TRUE,], MARINADE_VALIDATORS_COUNT)
min_score_in_algo_set <- min(validators_algo_set$score)

# Mark validator who should receive algo stake
validators$in_algo_stake_set <- FALSE
validators$in_algo_stake_set[validators$score >= min_score_in_algo_set] <- TRUE
validators$in_algo_stake_set[validators$eligible_algo_stake == FALSE] <- FALSE

# Apply mnde staking eligibility criteria
validators$eligible_mnde_stake <- TRUE
validators$eligible_mnde_stake[validators$max_commission > ELIGIBILITY_MNDE_STAKE_MAX_COMMISSION] <- FALSE
validators$eligible_mnde_stake[validators$minimum_stake < ELIGIBILITY_MNDE_STAKE_MIN_STAKE] <- FALSE
validators$eligible_mnde_stake[validators$score < min_score_in_algo_set * ELIGIBILITY_MNDE_SCORE_THRESHOLD_MULTIPLIER] <- FALSE

# Apply eligibility on votes to get effective votes
mnde_valid_votes <- round(validators$mnde_votes * validators$eligible_mnde_stake / 1e9)

# Apply cap on the share of mnde votes
mnde_power_cap <- round(sum(mnde_valid_votes) * MNDE_VALIDATOR_CAP)
validators$mnde_power <- pmin(mnde_valid_votes, mnde_power_cap)

# Find out how much votes got truncated
mnde_overflow <- sum(mnde_valid_votes) - sum(validators$mnde_power)

# Sort validators by MNDE power
validators <- validators[order(validators$mnde_power, decreasing = T),]

# Distribute the overflow from the capping
for (v in 1:length(validators$mnde_power)) {
  validators_index <- seq(1, along.with = validators$mnde_power)
  # Ignore weights of already processed validators as they 1) already received their share from the overflow; 2) were overflowing
  moving_weights <- (validators_index > v - 1) * validators$mnde_power
  # Break the loop if no one else should receive stake from the mnde voting
  if (sum(moving_weights) == 0) {
    break
  }
  # How much should the power increase from the overflow
  mnde_power_increase <- round(mnde_overflow * moving_weights[v] / sum(moving_weights))
  # Limit the increase of mnde power if cap should be applied
  mnde_power_increase_capped <- min(mnde_power_increase, mnde_power_cap - moving_weights[v])
  # Increase mnde power for this validator
  validators$mnde_power <- validators$mnde_power + (validators_index == v) * mnde_power_increase_capped
  # Reduce the overflow by what was given to this validator
  mnde_overflow <- mnde_overflow - mnde_power_increase_capped
}

# Scale mnde power to a percentage
if (sum(validators$mnde_power) > 0) {
  total_mnde_power <- sum(validators$mnde_power, mnde_overflow)
  validators$mnde_power <- validators$mnde_power / total_mnde_power
  mnde_overflow_power <- mnde_overflow / total_mnde_power
} else {
  mnde_overflow_power <- 1
}

STAKE_CONTROL_MNDE_SOL <- TOTAL_STAKE * STAKE_CONTROL_MNDE
STAKE_CONTROL_MNDE_OVERFLOW_SOL <- mnde_overflow_power * STAKE_CONTROL_MNDE_SOL
STAKE_CONTROL_ALGO_SOL <- TOTAL_STAKE * STAKE_CONTROL_ALGO + STAKE_CONTROL_MNDE_OVERFLOW_SOL

validators$target_stake_mnde <- round(validators$mnde_power * STAKE_CONTROL_MNDE_SOL)
validators$target_stake_algo <- round(validators$score * validators$in_algo_stake_set / sum(validators$score * validators$in_algo_stake_set) * STAKE_CONTROL_ALGO_SOL)
validators$target_stake <- validators$target_stake_mnde + validators$target_stake_algo + validators$target_stake_self

perf_target_stake_mnde <- sum(validators$avg_adjusted_credits * validators$target_stake_mnde) / sum(validators$target_stake_mnde)
perf_target_stake_algo <- sum(validators$avg_adjusted_credits * validators$target_stake_algo) / sum(validators$target_stake_algo)
perf_target_stake_self <- sum(validators$avg_adjusted_credits * validators$target_stake_self) / sum(validators$target_stake_self)

t(data.frame(
  TOTAL_STAKE,
  STAKE_CONTROL_SELF_STAKE_SOL,
  STAKE_CONTROL_SELF_STAKE_OVERFLOW_SOL,
  STAKE_CONTROL_MNDE_SOL,
  STAKE_CONTROL_MNDE_OVERFLOW_SOL,
  STAKE_CONTROL_ALGO_SOL,
  perf_target_stake_mnde,
  perf_target_stake_algo,
  perf_target_stake_self
))

validators <- validators[order(validators$rank),]
write.csv(validators, file_out_scores)

write.csv(validators[order(validators$target_stake, decreasing = T),][validators$target_stake > 0,], file_out_stakes)
