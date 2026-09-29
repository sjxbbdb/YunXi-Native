//! 云熙星空动效演示
//!
//! 运行: cargo run -p yunxi-agent-tui --example starfield_demo

use yunxi_agent_tui::yunxi_starfield::*;

fn main() {
    println!("🌸 云熙星空动效演示\n");

    // 显示配色
    println!("【云汐角色配色】");
    println!(
        "银白色:     #{:02X}{:02X}{:02X}",
        YUNXI_SILVER.0, YUNXI_SILVER.1, YUNXI_SILVER.2
    );
    println!(
        "柔和紫色:   #{:02X}{:02X}{:02X}",
        YUNXI_PURPLE.0, YUNXI_PURPLE.1, YUNXI_PURPLE.2
    );
    println!(
        "薰衣草:     #{:02X}{:02X}{:02X}",
        YUNXI_LAVENDER.0, YUNXI_LAVENDER.1, YUNXI_LAVENDER.2
    );
    println!(
        "服饰白:     #{:02X}{:02X}{:02X}",
        YUNXI_WHITE.0, YUNXI_WHITE.1, YUNXI_WHITE.2
    );
    println!(
        "紫粉色:     #{:02X}{:02X}{:02X}\n",
        YUNXI_PINK.0, YUNXI_PINK.1, YUNXI_PINK.2
    );

    // 显示艺术字
    let art = BannerArt::yunxi_builtin(false);
    println!("【云熙艺术字】 {}x{}", art.cols(), art.rows());
    for line in &art.lines {
        println!("{}", line);
    }
    println!("\n副标题: {}\n", art.subtitle);

    // 演示星空动画
    println!("【星空动画演示】（10帧）");
    for frame in 0..10 {
        print!("第 {:2} 帧: ", frame);
        for x in 0..40 {
            if let Some((glyph, _bright)) = star_at(x, 5, frame, false, 8) {
                print!("{}", glyph);
            } else {
                print!(" ");
            }
        }
        println!();
    }

    println!("\n✅ 星空模块工作正常！");
}
