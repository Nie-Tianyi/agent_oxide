//! InventoryAgent — the NVIDIA OO Agents quickstart/03 equivalent.
//!
//! The key claim being validated: **synchronous methods are tools**. During
//! the CodeAct run, the LLM must call `get_stock` / `get_price` (registered
//! automatically by `#[agent_impl]`) to answer the order question — the
//! inventory data exists nowhere else, so hallucinated numbers fail the
//! assertions.
//!
//! Also exercises `#[strategy(code_act, max_iterations = 15)]` (Phase 3)
//! and structured output with auto-retry (`OrderResult`).
//!
//! ```text
//! cargo run -p agent-kit --example inventory_agent
//! ```
//! (requires `DEEPSEEK_API`; blueprint checks run without it)

use std::collections::HashMap;

use agent_kit::schemars::JsonSchema;
use agent_kit::serde::{Deserialize, Serialize};
use agent_macros::{Agent, agent_impl};
use deepseek::DeepSeekClient;

/// 你是一个库存管理 Agent。
///
/// 你可以查询仓库中每个物品的库存数量和单价，然后判断一个订单是否能在预算内完成。
/// 回答订单问题时，必须先调用工具查询实际数据，绝不能凭空猜测。
#[derive(Clone, Agent)]
struct InventoryAgent {
    #[agent(client)]
    client: DeepSeekClient,
    inventory: HashMap<String, Item>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(crate = "agent_kit::serde")]
#[schemars(crate = "agent_kit::schemars")]
struct Item {
    name: String,
    stock: i32,
    price: f64,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(crate = "agent_kit::serde")]
#[schemars(crate = "agent_kit::schemars")]
struct OrderResult {
    can_fulfill: bool,
    total_cost: f64,
    unavailable_items: Vec<String>,
}

#[agent_impl]
impl InventoryAgent {
    /// 获取指定物品的当前库存数量。
    fn get_stock(&self, item: String) -> i32 {
        self.inventory.get(&item).map(|i| i.stock).unwrap_or(0)
    }

    /// 获取指定物品的当前单价。
    fn get_price(&self, item: String) -> f64 {
        self.inventory.get(&item).map(|i| i.price).unwrap_or(0.0)
    }

    /// 检查订单是否可以在预算内完成：是否所有物品都有货、总价是否在预算内。
    /// 返回是否可完成、总价、以及缺货的物品列表。
    /// 你必须使用 get_stock 和 get_price 工具逐一查询每个物品的真实数据，
    /// 不能猜测或编造库存和价格。
    #[strategy(code_act, max_iterations = 15)]
    async fn can_fulfill_order(&self, items: Vec<String>, budget: f64) -> OrderResult {}
}

fn make_agent(api_key: impl Into<String>) -> InventoryAgent {
    let inventory = HashMap::from([
        (
            "apple".to_string(),
            Item {
                name: "apple".into(),
                stock: 5,
                price: 3.0,
            },
        ),
        (
            "banana".to_string(),
            Item {
                name: "banana".into(),
                stock: 0,
                price: 1.0,
            },
        ),
        (
            "gold".to_string(),
            Item {
                name: "gold".into(),
                stock: 1,
                price: 999.0,
            },
        ),
    ]);
    InventoryAgent {
        client: DeepSeekClient::new(api_key),
        inventory,
    }
}

fn validate_blueprint(agent: &InventoryAgent) {
    // Both sync methods are auto-registered as tools with schemas derived
    // from their parameter lists.
    let mut registry = agent_kit::tools::ToolRegistry::new();
    agent_kit::AgentBlueprint::blueprint_register_tools(agent, &mut registry);
    for name in ["get_stock", "get_price"] {
        let tool = registry
            .get(name)
            .unwrap_or_else(|| panic!("{name} registered as tool"));
        let schema = tool.parameter_schema();
        assert!(
            schema["properties"]["item"].is_object(),
            "{name} args schema"
        );
    }

    // Direct Rust calls still work (the original methods are preserved).
    assert_eq!(agent.get_stock("apple".into()), 5);
    assert_eq!(agent.get_price("gold".into()), 999.0);

    println!("[ok] blueprint: get_stock/get_price registered with derived schemas");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = match std::env::var("DEEPSEEK_API") {
        Ok(key) => key,
        Err(_) => {
            println!("DEEPSEEK_API not set — blueprint validation only.");
            validate_blueprint(&make_agent("sk-test"));
            println!("example finished (set DEEPSEEK_API to run the live order check)");
            return Ok(());
        }
    };

    let agent = make_agent(api_key);
    validate_blueprint(&agent);

    // Order: 1 apple (¥3, in stock) + 1 banana (¥1, OUT OF STOCK) +
    // 1 gold bar (¥999, in stock) — with a budget of ¥10 the order must
    // fail: banana unavailable, gold blows the budget. The only way the
    // LLM gets these numbers right is by calling the tools.
    let result = agent
        .can_fulfill_order(vec!["apple".into(), "banana".into(), "gold".into()], 10.0)
        .await?;
    println!("[llm] can_fulfill_order: {result:?}");

    assert!(!result.can_fulfill, "gold costs 999 > budget 10");
    assert!(
        (result.total_cost - 1003.0).abs() < 1.0,
        "total must reflect tool-queried prices, got {}",
        result.total_cost
    );
    assert!(
        result.unavailable_items.iter().any(|i| i == "banana"),
        "banana has stock 0 → must be reported unavailable, got {:?}",
        result.unavailable_items
    );

    println!("[ok] live: LLM called the sync-method tools and computed the correct answer");
    println!("example finished");
    Ok(())
}
