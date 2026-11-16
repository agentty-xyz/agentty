//! Model catalog data: identifiers, specifications, capabilities, and list
//! prices.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model_profile::ModelProfile;
use crate::provider::{CostTier, ModelProvider};

/// One million micro-USD, one US dollar.
const MICRO_USD_PER_USD: u64 = 1_000_000;
/// Highest output price, per million tokens, of a [`CostTier::Cheap`] model.
const CHEAP_MAX_OUTPUT_MICRO_USD: u64 = 2_500_000;
/// Lowest output price, per million tokens, of a [`CostTier::Premium`] model.
const PREMIUM_MIN_OUTPUT_MICRO_USD: u64 = 15 * MICRO_USD_PER_USD;

/// Catalog key of one model, unique within a [`super::ModelCatalog`].
///
/// Built-in entries use `provider/model`, such as `openai/gpt-6-luna`, so a
/// model served by two providers, such as an `OpenAI` model reached with an
/// API key or through a `ChatGPT` plan, has two distinct identifiers.
#[derive(
    Clone, Debug, Deserialize, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(transparent)]
pub struct ModelId(String);

impl ModelId {
    /// Creates an identifier from any string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns the identifier of the built-in entry for `model` served by
    /// `provider`, such as `qwen/qwen3.8-flash`.
    pub fn cataloged(provider: ModelProvider, model: &str) -> Self {
        Self(format!("{provider}/{model}"))
    }

    /// Returns the identifier text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<&str> for ModelId {
    fn from(id: &str) -> Self {
        Self::new(id)
    }
}

impl From<String> for ModelId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

/// What a cataloged model can do, beyond the text and locally validated
/// structured output every model provides.
///
/// Capabilities describe the model; they do not enable tools or features.
/// Start from [`Self::NONE`] and add each capability with its `with_` method.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub struct Capabilities {
    flags: u8,
}

impl Capabilities {
    const IMAGE_INPUT: u8 = 1;
    /// Every capability flag with its display name, in display order.
    const NAMES: [(u8, &str); 4] = [
        (Self::IMAGE_INPUT, "image_input"),
        (Self::REASONING, "reasoning"),
        (Self::STRUCTURED_OUTPUT, "structured_output"),
        (Self::TOOL_CALLS, "tool_calls"),
    ];
    /// No optional capability.
    pub const NONE: Self = Self { flags: 0 };
    const REASONING: u8 = 1 << 1;
    const STRUCTURED_OUTPUT: u8 = 1 << 2;
    const TOOL_CALLS: u8 = 1 << 3;

    /// Sets whether the model reads image input.
    #[must_use]
    pub const fn with_image_input(self, image_input: bool) -> Self {
        self.with(Self::IMAGE_INPUT, image_input)
    }

    /// Sets whether the model accepts a reasoning-effort setting.
    #[must_use]
    pub const fn with_reasoning(self, reasoning: bool) -> Self {
        self.with(Self::REASONING, reasoning)
    }

    /// Sets whether the provider constrains output to the JSON schema
    /// natively, instead of stating the schema in the prompt.
    #[must_use]
    pub const fn with_structured_output(self, structured_output: bool) -> Self {
        self.with(Self::STRUCTURED_OUTPUT, structured_output)
    }

    /// Sets whether the model makes native tool calls.
    #[must_use]
    pub const fn with_tool_calls(self, tool_calls: bool) -> Self {
        self.with(Self::TOOL_CALLS, tool_calls)
    }

    /// Returns whether the model reads image input.
    pub const fn image_input(self) -> bool {
        self.has(Self::IMAGE_INPUT)
    }

    /// Returns whether the model accepts a reasoning-effort setting.
    pub const fn reasoning(self) -> bool {
        self.has(Self::REASONING)
    }

    /// Returns whether the provider constrains output to the JSON schema
    /// natively.
    pub const fn structured_output(self) -> bool {
        self.has(Self::STRUCTURED_OUTPUT)
    }

    /// Returns whether the model makes native tool calls.
    pub const fn tool_calls(self) -> bool {
        self.has(Self::TOOL_CALLS)
    }

    /// Returns whether these capabilities include every capability `needs`
    /// asks for.
    pub const fn satisfies(self, needs: Self) -> bool {
        self.has(needs.flags)
    }

    const fn with(mut self, flag: u8, set: bool) -> Self {
        if set {
            self.flags |= flag;
        } else {
            self.flags &= !flag;
        }

        self
    }

    const fn has(self, flags: u8) -> bool {
        self.flags & flags == flags
    }
}

impl fmt::Display for Capabilities {
    /// Lists the set capabilities, such as `image_input, tool_calls`, or
    /// `none`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = Self::NAMES
            .into_iter()
            .filter_map(|(flag, name)| self.has(flag).then_some(name))
            .collect::<Vec<_>>();
        if names.is_empty() {
            return formatter.write_str("none");
        }

        formatter.write_str(&names.join(", "))
    }
}

/// Where a [`Pricing`] came from.
///
/// On the wire it is `built_in` or `host`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PricingSource {
    /// The harness's built-in list prices, as the providers published them
    /// on 2026-10-29 at their lowest input tier.
    BuiltIn,
    /// Prices the host supplied, such as a negotiated rate.
    Host,
}

/// Token prices of one model in integer micro-USD per million tokens.
///
/// Integer micro-USD keep cost arithmetic exact; [`crate::Cost::of`] prices
/// a [`crate::Usage`] with them. Cached input bills at
/// [`Self::cache_read_per_mtok_micro_usd`] and cache creation at
/// [`Self::cache_write_per_mtok_micro_usd`]; without a declared cache price,
/// both fall back to the input price. Built-in prices do not model cache
/// discounts yet, so they overstate cached input for providers that publish
/// one; a host can declare those prices through
/// [`super::CatalogBuilder::pricing`] or [`crate::HarnessBuilder::pricing`].
///
/// Changing a cache price marks the prices [`PricingSource::Host`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub struct Pricing {
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    input: u64,
    output: u64,
    source: PricingSource,
}

impl Pricing {
    /// Declares host prices for input and output tokens, in micro-USD per
    /// million tokens, with no separate cache prices.
    pub const fn new(input_per_mtok_micro_usd: u64, output_per_mtok_micro_usd: u64) -> Self {
        Self {
            cache_read: None,
            cache_write: None,
            input: input_per_mtok_micro_usd,
            output: output_per_mtok_micro_usd,
            source: PricingSource::Host,
        }
    }

    /// Sets the price of cached input tokens, marking the prices
    /// [`PricingSource::Host`].
    #[must_use]
    pub const fn with_cache_read(mut self, cache_read_per_mtok_micro_usd: u64) -> Self {
        self.cache_read = Some(cache_read_per_mtok_micro_usd);
        self.source = PricingSource::Host;

        self
    }

    /// Sets the price of input tokens written to the provider's cache,
    /// marking the prices [`PricingSource::Host`].
    #[must_use]
    pub const fn with_cache_write(mut self, cache_write_per_mtok_micro_usd: u64) -> Self {
        self.cache_write = Some(cache_write_per_mtok_micro_usd);
        self.source = PricingSource::Host;

        self
    }

    /// Returns the input price in micro-USD per million tokens.
    pub const fn input_per_mtok_micro_usd(self) -> u64 {
        self.input
    }

    /// Returns the output price, including reasoning tokens, in micro-USD
    /// per million tokens.
    pub const fn output_per_mtok_micro_usd(self) -> u64 {
        self.output
    }

    /// Returns the cached-input price in micro-USD per million tokens, the
    /// input price unless a separate price is declared.
    pub const fn cache_read_per_mtok_micro_usd(self) -> u64 {
        match self.cache_read {
            Some(price) => price,
            None => self.input,
        }
    }

    /// Returns the cache-write price in micro-USD per million tokens, the
    /// input price unless a separate price is declared.
    pub const fn cache_write_per_mtok_micro_usd(self) -> u64 {
        match self.cache_write {
            Some(price) => price,
            None => self.input,
        }
    }

    /// Returns where these prices came from.
    pub const fn source(self) -> PricingSource {
        self.source
    }

    /// Returns the price class of the output price: [`CostTier::Cheap`] up
    /// to $2.50 per million tokens, [`CostTier::Premium`] from $15, and
    /// [`CostTier::Standard`] between.
    pub const fn cost_tier(self) -> CostTier {
        if self.output <= CHEAP_MAX_OUTPUT_MICRO_USD {
            CostTier::Cheap
        } else if self.output >= PREMIUM_MIN_OUTPUT_MICRO_USD {
            CostTier::Premium
        } else {
            CostTier::Standard
        }
    }

    /// Returns built-in list prices in micro-USD per million tokens.
    pub(crate) const fn built_in(
        input_per_mtok_micro_usd: u64,
        output_per_mtok_micro_usd: u64,
    ) -> Self {
        Self::new(input_per_mtok_micro_usd, output_per_mtok_micro_usd)
            .with_source(PricingSource::BuiltIn)
    }

    /// Returns these prices attributed to `source`.
    pub(crate) const fn with_source(mut self, source: PricingSource) -> Self {
        self.source = source;

        self
    }
}

/// Description of one cataloged model: identity, provider, context,
/// capabilities, prices, and concurrency limit.
///
/// Start with [`Self::new`] and add what is known; unset fields stay
/// unknown rather than defaulting to a guess.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ModelSpec {
    capabilities: Capabilities,
    id: ModelId,
    max_concurrency: Option<u32>,
    max_output: Option<u64>,
    pricing: Option<Pricing>,
    profile: Option<ModelProfile>,
    provider: Option<ModelProvider>,
}

impl ModelSpec {
    /// Describes the model `id` with no capabilities and nothing else known.
    pub fn new(id: impl Into<ModelId>) -> Self {
        Self {
            capabilities: Capabilities::NONE,
            id: id.into(),
            max_concurrency: None,
            max_output: None,
            pricing: None,
            profile: None,
            provider: None,
        }
    }

    /// Sets the built-in provider serving the model.
    #[must_use]
    pub fn with_provider(mut self, provider: ModelProvider) -> Self {
        self.provider = Some(provider);

        self
    }

    /// Declares the model's context profile, from which the catalog derives
    /// its request budget.
    #[must_use]
    pub fn with_profile(mut self, profile: ModelProfile) -> Self {
        self.profile = Some(profile);

        self
    }

    /// Declares the most tokens one response may contain.
    #[must_use]
    pub fn with_max_output(mut self, max_output_tokens: u64) -> Self {
        self.max_output = Some(max_output_tokens);

        self
    }

    /// Declares what the model can do.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;

        self
    }

    /// Declares the model's token prices.
    #[must_use]
    pub fn with_pricing(mut self, pricing: Pricing) -> Self {
        self.pricing = Some(pricing);

        self
    }

    /// Declares the most requests the host should run on this one model at
    /// once.
    ///
    /// The cap is per model. A limit shared by every model of one provider,
    /// such as a `ChatGPT` plan's rate limit, is not expressible here.
    #[must_use]
    pub fn with_max_concurrency(mut self, max_concurrency: u32) -> Self {
        self.max_concurrency = Some(max_concurrency);

        self
    }

    /// Returns the catalog key.
    pub fn id(&self) -> &ModelId {
        &self.id
    }

    /// Returns the built-in provider, or `None` for a host-implemented
    /// model.
    pub fn provider(&self) -> Option<ModelProvider> {
        self.provider
    }

    /// Returns the declared context profile, if known.
    pub fn profile(&self) -> Option<ModelProfile> {
        self.profile
    }

    /// Returns the total context window in tokens, from the profile.
    pub fn context_window(&self) -> Option<u64> {
        self.profile.map(ModelProfile::context_window_tokens)
    }

    /// Returns the most tokens one response may contain, if known.
    pub fn max_output(&self) -> Option<u64> {
        self.max_output
    }

    /// Returns what the model can do.
    pub fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    /// Returns the token prices, or `None` when unknown or not billed per
    /// token, such as for a `ChatGPT` plan or a local server.
    pub fn pricing(&self) -> Option<Pricing> {
        self.pricing
    }

    /// Returns the most concurrent requests the host should run on this
    /// model, if limited.
    pub fn max_concurrency(&self) -> Option<u32> {
        self.max_concurrency
    }
}

#[cfg(test)]
#[path = "spec_test.rs"]
mod tests;
