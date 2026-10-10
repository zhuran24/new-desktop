//! 原生场景的只读控件几何，作用等同于浏览器测试读取已布局的元素矩形。
use gpui_kit::*;

pub(crate) trait ScenarioBounds {
    fn scenario_bounds(self, id: impl Into<String>) -> Self;
}

impl ScenarioBounds for Stateful<Div> {
    fn scenario_bounds(self, id: impl Into<String>) -> Self {
        #[cfg(not(feature = "scenarios"))]
        {
            let _ = id;
            self
        }
        #[cfg(feature = "scenarios")]
        {
            let id = id.into();
            self.relative().child(
                canvas(
                    |_, _, _| (),
                    move |b, (), _, _| {
                        println!(
                            "{}",
                            serde_json::json!({"scenario_bounds":{"id":id,"rect":{
                        "x":f32::from(b.origin.x),"y":f32::from(b.origin.y),
                        "width":f32::from(b.size.width),"height":f32::from(b.size.height)}}})
                        );
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
        }
    }
}
