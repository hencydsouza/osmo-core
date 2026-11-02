pub mod advert;
pub mod model;

pub use advert::{decode_advert, is_dji_company_id, model_name, Advert};
pub use model::{resolve_model, CameraModel};
