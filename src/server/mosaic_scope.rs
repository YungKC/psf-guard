//! A project's mosaic panels as one scope, so the Library can show a
//! Target Scheduler mosaic's targets together. Grading and calibration stay
//! per panel; Stacks can integrate their lights on an expanded mosaic canvas.
//!
//! The panels come from Director's records when it activated the project,
//! else from the grid the targets' coordinates form. Target names such as
//! "r1c2" are never read for this. Director's panels need not form a grid:
//! panels placed one by one, or a panel moved off its old target, are
//! listed in the rows and columns their targets fall on.
//!
//! User-defined mosaics (fork: manual mosaic) are a third kind of scope:
//! a named selection of targets within one project, stored in
//! `psf_guard_mosaic` / `psf_guard_mosaic_panel`. A user mosaic never
//! duplicates frames and never changes a frame's project or target; it
//! only names the panels a mosaic stack should integrate. The default
//! project mosaic stays Director, then inferred, then the first user
//! mosaic when neither of the first two exists; a named user mosaic is
//! returned when the caller asks for it by id.

use super::api::ApiResponse;
use super::extract::DbContext;
use super::handlers::AppError;
use super::sky_coverage::{previews_for, SkyPreview};
use axum::{
    extract::{Path, Query},
    Json,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Where a mosaic's panel layout came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MosaicSource {
    /// Director's activation record names each target's panel.
    Director,
    /// The targets' coordinates and rotation form the grid.
    Inferred,
    /// A person picked these panels in this project.
    User,
}

#[derive(Debug, Clone, Serialize)]
pub struct MosaicPanel {
    pub target_id: i32,
    pub target_name: String,
    /// Director's panel id (`r{row}c{column}` for a grid's panel, `p{n}`
    /// for one placed since), or `r{row}c{column}` for an inferred grid.
    pub panel_id: String,
    /// One-based, row 1 at the top and column 1 at the left. For panels
    /// that form no full grid, the sparse grid their targets fall on.
    pub row: u32,
    pub column: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<SkyPreview>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectMosaic {
    pub project_id: i32,
    pub name: String,
    pub source: MosaicSource,
    pub rows: u32,
    pub columns: u32,
    /// Row by row from the top, left to right.
    pub panels: Vec<MosaicPanel>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectMosaicResponse {
    /// `None` when the project is not a mosaic.
    pub mosaic: Option<ProjectMosaic>,
}

/// A user-defined mosaic, as stored and as listed. Its panels use the
/// same shape as `ProjectMosaic` so stack code reads either one.
#[derive(Debug, Clone, Serialize)]
pub struct UserMosaic {
    pub id: i64,
    pub project_id: i32,
    pub name: String,
    pub source: MosaicSource,
    pub rows: u32,
    pub columns: u32,
    pub panels: Vec<MosaicPanel>,
}

/// One panel of a create request. Row and column are one-based and
/// optional: when either is missing, the panel is placed from its
/// target's coordinates, sparse grids allowed.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateMosaicPanel {
    pub target_id: i32,
    pub row: Option<u32>,
    pub column: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateMosaicRequest {
    pub name: String,
    pub panels: Vec<CreateMosaicPanel>,
}

/// One target, with the detail a mosaic picker needs: its filters and
/// frame counts, plus rig and field of view read header-only from its
/// first light when that frame can be read. Header reads never edit a
/// file; a target whose first frame cannot be read still lists, with
/// rig and field of view empty.
#[derive(Debug, Clone, Serialize)]
pub struct MosaicTargetDetail {
    pub target_id: i32,
    pub name: String,
    pub ra_hours: Option<f64>,
    pub dec_degrees: Option<f64>,
    pub filters: Vec<MosaicTargetFilter>,
    pub telescope: Option<String>,
    pub camera: Option<String>,
    pub width_px: Option<i64>,
    pub height_px: Option<i64>,
    pub fov_width_degrees: Option<f64>,
    pub fov_height_degrees: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MosaicTargetFilter {
    pub filter_name: String,
    pub frames: i64,
    pub accepted: i64,
}

const USER_MOSAIC_DDL: &str = "
CREATE TABLE IF NOT EXISTS psf_guard_mosaic (
    Id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS psf_guard_mosaic_panel (
    mosaic_id INTEGER NOT NULL,
    target_id INTEGER NOT NULL,
    row INTEGER NOT NULL,
    column INTEGER NOT NULL,
    panel_id TEXT NOT NULL,
    PRIMARY KEY (mosaic_id, target_id)
);
";

pub(crate) fn ensure_user_mosaic_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(USER_MOSAIC_DDL)
}

fn user_mosaic_from_rows(
    conn: &Connection,
    id: i64,
    project_id: i32,
    name: String,
) -> rusqlite::Result<UserMosaic> {
    let names = target_names(conn, project_id)?;
    let mut statement = conn.prepare(
        "SELECT target_id, row, column, panel_id FROM psf_guard_mosaic_panel
         WHERE mosaic_id = ?1 ORDER BY row, column, target_id",
    )?;
    let rows = statement.query_map([id], |row| {
        Ok((
            row.get::<_, i32>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, u32>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut panels = Vec::new();
    for row in rows {
        let (target_id, row, column, panel_id) = row?;
        panels.push(MosaicPanel {
            target_id,
            target_name: names.get(&target_id).cloned().unwrap_or_default(),
            panel_id,
            row,
            column,
            preview: None,
        });
    }
    let rows_max = panels.iter().map(|panel| panel.row).max().unwrap_or(0);
    let columns_max = panels.iter().map(|panel| panel.column).max().unwrap_or(0);
    Ok(UserMosaic {
        id,
        project_id,
        name,
        source: MosaicSource::User,
        rows: rows_max,
        columns: columns_max,
        panels,
    })
}

pub(crate) fn list_user_mosaics(
    conn: &Connection,
    project_id: i32,
) -> Result<Vec<UserMosaic>, AppError> {
    ensure_user_mosaic_tables(conn).map_err(AppError::db)?;
    let mut statement = conn
        .prepare("SELECT Id, name FROM psf_guard_mosaic WHERE project_id = ?1 ORDER BY Id")
        .map_err(AppError::db)?;
    let rows = statement
        .query_map([project_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(AppError::db)?;
    let mut mosaics = Vec::new();
    for row in rows {
        let (id, name) = row.map_err(AppError::db)?;
        mosaics.push(user_mosaic_from_rows(conn, id, project_id, name).map_err(AppError::db)?);
    }
    Ok(mosaics)
}

pub(crate) fn get_user_mosaic(
    conn: &Connection,
    project_id: i32,
    mosaic_id: i64,
) -> Result<Option<UserMosaic>, AppError> {
    ensure_user_mosaic_tables(conn).map_err(AppError::db)?;
    let name = conn
        .query_row(
            "SELECT name FROM psf_guard_mosaic WHERE Id = ?1 AND project_id = ?2",
            params![mosaic_id, project_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(AppError::db)?;
    match name {
        None => Ok(None),
        Some(name) => Ok(Some(
            user_mosaic_from_rows(conn, mosaic_id, project_id, name).map_err(AppError::db)?,
        )),
    }
}

/// Place panels from their targets' coordinates, reusing the sparse
/// placement Director panels use. The returned row and column are
/// one-based; a panel's supplied row and column, when both are given,
/// win over the placed ones, and a collision after that merge is an
/// error, not a silent reshuffle.
fn auto_place_panels(conn: &Connection, target_ids: &[i32]) -> rusqlite::Result<Vec<(u32, u32)>> {
    let rows: Vec<(i32, String, String)> = target_ids
        .iter()
        .enumerate()
        .map(|(index, target_id)| (*target_id, String::new(), format!("p{}", index + 1)))
        .collect();
    let placed = placed_cells(conn, &rows)?;
    let mut by_target: HashMap<i32, (u32, u32)> = HashMap::new();
    for cell in placed {
        by_target.insert(cell.target_id, (cell.row, cell.column));
    }
    Ok(target_ids
        .iter()
        .map(|target_id| by_target.get(target_id).copied().unwrap_or((1, 1)))
        .collect())
}

pub(crate) fn create_user_mosaic(
    conn: &Connection,
    project_id: i32,
    request: &CreateMosaicRequest,
) -> Result<UserMosaic, AppError> {
    ensure_user_mosaic_tables(conn).map_err(AppError::db)?;
    let name = request.name.trim().to_owned();
    if name.is_empty() {
        return Err(AppError::BadRequest("A mosaic needs a name".into()));
    }
    if request.panels.len() < 2 {
        return Err(AppError::BadRequest(
            "A mosaic needs at least two panels".into(),
        ));
    }
    let mut seen: HashSet<i32> = HashSet::new();
    for panel in &request.panels {
        if !seen.insert(panel.target_id) {
            return Err(AppError::BadRequest(format!(
                "Target {} is listed twice in one mosaic",
                panel.target_id
            )));
        }
    }
    // Every panel must be a target of this project, not of another one.
    for panel in &request.panels {
        let owner: Option<i32> = conn
            .query_row(
                "SELECT projectid FROM target WHERE Id = ?1",
                [panel.target_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(AppError::db)?;
        match owner {
            None => {
                return Err(AppError::BadRequest(format!(
                    "Target {} does not exist",
                    panel.target_id
                )))
            }
            Some(owner) if owner != project_id => {
                return Err(AppError::BadRequest(format!(
                    "Target {} does not belong to project {}",
                    panel.target_id, project_id
                )))
            }
            _ => {}
        }
    }
    let project_exists: Option<i32> = conn
        .query_row(
            "SELECT Id FROM project WHERE Id = ?1",
            [project_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(AppError::db)?;
    if project_exists.is_none() {
        return Err(AppError::NotFound);
    }
    let target_ids: Vec<i32> = request.panels.iter().map(|panel| panel.target_id).collect();
    let placed = auto_place_panels(conn, &target_ids).map_err(AppError::db)?;
    let mut placed_cells_set: HashSet<(u32, u32)> = HashSet::new();
    let mut resolved: Vec<(i32, u32, u32)> = Vec::new();
    for (index, panel) in request.panels.iter().enumerate() {
        let (row, column) = match (panel.row, panel.column) {
            (Some(row), Some(column)) if row >= 1 && column >= 1 => (row, column),
            (Some(_), Some(_)) => {
                return Err(AppError::BadRequest(
                    "Mosaic rows and columns are one-based".into(),
                ))
            }
            _ => placed[index],
        };
        if !placed_cells_set.insert((row, column)) {
            return Err(AppError::BadRequest(format!(
                "Two panels share row {} column {}",
                row, column
            )));
        }
        resolved.push((panel.target_id, row, column));
    }
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO psf_guard_mosaic (project_id, name, created_at) VALUES (?1, ?2, ?3)",
        params![project_id, name, created_at],
    )
    .map_err(AppError::db)?;
    let mosaic_id = conn.last_insert_rowid();
    for (target_id, row, column) in &resolved {
        conn.execute(
            "INSERT INTO psf_guard_mosaic_panel (mosaic_id, target_id, row, column, panel_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                mosaic_id,
                target_id,
                row,
                column,
                format!("r{}c{}", row, column)
            ],
        )
        .map_err(AppError::db)?;
    }
    user_mosaic_from_rows(conn, mosaic_id, project_id, name).map_err(AppError::db)
}

/// Delete one user mosaic. Only the mosaic and its panel rows go;
/// frames, targets, grades, and calibration are untouched.
pub(crate) fn delete_user_mosaic(
    conn: &Connection,
    project_id: i32,
    mosaic_id: i64,
) -> Result<bool, AppError> {
    ensure_user_mosaic_tables(conn).map_err(AppError::db)?;
    let deleted = conn
        .execute(
            "DELETE FROM psf_guard_mosaic_panel WHERE mosaic_id = ?1
             AND EXISTS (SELECT 1 FROM psf_guard_mosaic WHERE Id = ?1 AND project_id = ?2)",
            params![mosaic_id, project_id],
        )
        .map_err(AppError::db)?;
    let rows = conn
        .execute(
            "DELETE FROM psf_guard_mosaic WHERE Id = ?1 AND project_id = ?2",
            params![mosaic_id, project_id],
        )
        .map_err(AppError::db)?;
    let _ = deleted;
    Ok(rows > 0)
}

pub(crate) fn mosaic_target_details(
    conn: &Connection,
    project_id: i32,
) -> Result<Vec<MosaicTargetDetail>, AppError> {
    let mut statement = conn
        .prepare("SELECT Id, name, ra, dec FROM target WHERE projectid = ?1 ORDER BY Id")
        .map_err(AppError::db)?;
    let targets = statement
        .query_map([project_id], |row| {
            Ok((
                row.get::<_, i32>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<f64>>(2)?,
                row.get::<_, Option<f64>>(3)?,
            ))
        })
        .map_err(AppError::db)?;
    let mut details = Vec::new();
    for target in targets {
        let (target_id, name, ra_hours, dec_degrees) = target.map_err(AppError::db)?;
        let mut filter_statement = conn
            .prepare(
                "SELECT filtername, COUNT(*),
                 SUM(CASE WHEN gradingStatus = 1 THEN 1 ELSE 0 END)
                 FROM acquiredimage WHERE targetId = ?1 GROUP BY filtername ORDER BY filtername",
            )
            .map_err(AppError::db)?;
        let filter_rows = filter_statement
            .query_map([target_id], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            })
            .map_err(AppError::db)?;
        let mut filters = Vec::new();
        for filter_row in filter_rows {
            let (filter_name, frames, accepted) = filter_row.map_err(AppError::db)?;
            filters.push(MosaicTargetFilter {
                filter_name: filter_name.unwrap_or_default(),
                frames,
                accepted: accepted.unwrap_or(0),
            });
        }
        // Best-effort, header-only rig and field of view from the first
        // light of this target. A failure here lists the target without
        // rig detail rather than failing the picker.
        let (telescope, camera, width_px, height_px, fov_width, fov_height) =
            first_light_rig(conn, target_id);
        details.push(MosaicTargetDetail {
            target_id,
            name,
            ra_hours,
            dec_degrees,
            filters,
            telescope,
            camera,
            width_px,
            height_px,
            fov_width_degrees: fov_width,
            fov_height_degrees: fov_height,
        });
    }
    Ok(details)
}

fn first_light_rig(
    conn: &Connection,
    target_id: i32,
) -> (
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<f64>,
    Option<f64>,
) {
    let none = (None, None, None, None, None, None);
    let path: Option<String> = conn
        .query_row(
            "SELECT json_extract(metadata, '$.FileName') FROM acquiredimage
             WHERE targetId = ?1 ORDER BY Id LIMIT 1",
            [target_id],
            |row| row.get(0),
        )
        .ok()
        .flatten();
    let path = match path {
        Some(path) if !path.is_empty() => path,
        _ => return none,
    };
    let header = match crate::image_io::read_header(std::path::Path::new(&path)) {
        Ok(header) => header,
        Err(_) => return none,
    };
    let get = |key: &str| -> Option<String> {
        header
            .iter()
            .find(|(name, _)| name == key)
            .and_then(|(_, value)| match value {
                seiza_fits::HeaderValue::String(value) | seiza_fits::HeaderValue::Raw(value) => {
                    let value = value.trim();
                    (!value.is_empty()).then(|| value.to_owned())
                }
                _ => None,
            })
    };
    let number = |key: &str| -> Option<f64> {
        header
            .iter()
            .find(|(name, _)| name == key)
            .and_then(|(_, value)| value.as_f64())
            .filter(|value| value.is_finite())
    };
    let width = number("NAXIS1").map(|value| value as i64);
    let height = number("NAXIS2").map(|value| value as i64);
    let pixel_um = number("XPIXSZ").or_else(|| number("PIXSIZE1"));
    let focal_mm = number("FOCALLEN");
    let (fov_width, fov_height) = match (width, height, pixel_um, focal_mm) {
        (Some(w), Some(h), Some(pixel), Some(focal)) if focal > 0.0 => {
            let to_degrees =
                |pixels: i64| (pixels as f64 * pixel / 1000.0 / focal).atan().to_degrees();
            (Some(to_degrees(w)), Some(to_degrees(h)))
        }
        _ => (None, None),
    };
    (
        get("TELESCOP"),
        get("INSTRUME"),
        width,
        height,
        fov_width,
        fov_height,
    )
}

#[derive(Debug, Deserialize, Default)]
pub struct MosaicQuery {
    /// A user mosaic to return instead of the project's default mosaic.
    pub mosaic_id: Option<i64>,
}

/// GET /api/db/{db}/projects/{project_id}/mosaic
/// With `?mosaic_id=`, the named user mosaic; without it, the project's
/// default mosaic (Director, then inferred, then the first user mosaic
/// when neither of the first two exists).
pub async fn get_project_mosaic(
    ctx: DbContext,
    Path((_db_id, project_id)): Path<(String, i32)>,
    Query(query): Query<MosaicQuery>,
) -> Result<Json<ApiResponse<ProjectMosaicResponse>>, AppError> {
    let mosaic = {
        let conn = ctx.db();
        let conn = conn.lock().map_err(AppError::db)?;
        match query.mosaic_id {
            Some(mosaic_id) => project_mosaic_named(&conn, project_id, mosaic_id)?,
            None => project_mosaic(&conn, project_id)?,
        }
    };
    let Some(mut mosaic) = mosaic else {
        return Ok(Json(ApiResponse::success(ProjectMosaicResponse {
            mosaic: None,
        })));
    };
    let context = ctx.0.clone();
    let previews = tokio::task::spawn_blocking(move || previews_for(&context))
        .await
        .map_err(|error| AppError::InternalError(format!("mosaic preview task failed: {error}")))?;
    attach_previews(&mut mosaic, previews);
    Ok(Json(ApiResponse::success(ProjectMosaicResponse {
        mosaic: Some(mosaic),
    })))
}

fn attach_previews(mosaic: &mut ProjectMosaic, mut previews: HashMap<i32, SkyPreview>) {
    for panel in &mut mosaic.panels {
        panel.preview = previews.remove(&panel.target_id);
    }
}

/// GET /api/db/{db}/projects/{project_id}/mosaics — the project's
/// user-defined mosaics, in creation order.
pub async fn list_project_mosaics(
    ctx: DbContext,
    Path((_db_id, project_id)): Path<(String, i32)>,
) -> Result<Json<ApiResponse<Vec<UserMosaic>>>, AppError> {
    let mosaics = {
        let conn = ctx.db();
        let conn = conn.lock().map_err(AppError::db)?;
        list_user_mosaics(&conn, project_id)?
    };
    Ok(Json(ApiResponse::success(mosaics)))
}

/// POST /api/db/{db}/projects/{project_id}/mosaics — create a user
/// mosaic from picked targets. The mosaic stores only target ids and
/// grid positions; it never copies or moves a frame.
pub async fn create_project_mosaic(
    ctx: DbContext,
    Path((_db_id, project_id)): Path<(String, i32)>,
    Json(request): Json<CreateMosaicRequest>,
) -> Result<Json<ApiResponse<UserMosaic>>, AppError> {
    let mosaic = {
        let conn = ctx.db();
        let conn = conn.lock().map_err(AppError::db)?;
        create_user_mosaic(&conn, project_id, &request)?
    };
    Ok(Json(ApiResponse::success(mosaic)))
}

/// DELETE /api/db/{db}/projects/{project_id}/mosaics/{mosaic_id} —
/// remove one user mosaic. Frames, targets, grades, and calibration
/// are untouched; only the mosaic and its panel rows are deleted.
pub async fn delete_project_mosaic(
    ctx: DbContext,
    Path((_db_id, project_id, mosaic_id)): Path<(String, i32, i64)>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    let deleted = {
        let conn = ctx.db();
        let conn = conn.lock().map_err(AppError::db)?;
        delete_user_mosaic(&conn, project_id, mosaic_id)?
    };
    if !deleted {
        return Err(AppError::NotFound);
    }
    Ok(Json(ApiResponse::success(true)))
}

/// GET /api/db/{db}/projects/{project_id}/mosaic-targets — the detail
/// a mosaic picker lists per target: name, centre, filters and frame
/// counts, plus rig and field of view when its first light can be
/// read header-only.
pub async fn list_mosaic_targets(
    ctx: DbContext,
    Path((_db_id, project_id)): Path<(String, i32)>,
) -> Result<Json<ApiResponse<Vec<MosaicTargetDetail>>>, AppError> {
    let details = {
        let conn = ctx.db();
        let conn = conn.lock().map_err(AppError::db)?;
        mosaic_target_details(&conn, project_id)?
    };
    Ok(Json(ApiResponse::success(details)))
}

/// One user mosaic, by id, as a project mosaic. `None` when the id is
/// not a mosaic of this project.
pub(crate) fn project_mosaic_named(
    conn: &Connection,
    project_id: i32,
    mosaic_id: i64,
) -> Result<Option<ProjectMosaic>, AppError> {
    let Some(user) = get_user_mosaic(conn, project_id, mosaic_id)? else {
        return Ok(None);
    };
    Ok(Some(ProjectMosaic {
        project_id: user.project_id,
        name: user.name,
        source: MosaicSource::User,
        rows: user.rows,
        columns: user.columns,
        panels: user.panels,
    }))
}

/// The project's mosaic, Director's record first, then the inferred grid,
/// then the first user mosaic when neither of the first two exists. The
/// inferred grid stays the default for a project that has one, so a user
/// mosaic is a named alternative there, chosen with `?mosaic_id=`.
pub(crate) fn project_mosaic(
    conn: &Connection,
    project_id: i32,
) -> Result<Option<ProjectMosaic>, AppError> {
    let Some(project_name) = conn
        .query_row(
            "SELECT name FROM project WHERE Id=?1",
            [project_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(AppError::db)?
    else {
        return Ok(None);
    };
    let names = target_names(conn, project_id).map_err(AppError::db)?;
    let recorded = director_cells(conn, project_id).map_err(AppError::db)?;
    let (source, cells) = match recorded {
        Some(cells) => (MosaicSource::Director, Some(cells)),
        None => {
            let inferred = crate::server::director::inferred_mosaic(conn, i64::from(project_id))
                .map_err(|error| AppError::InternalError(error.to_string()))?;
            (
                MosaicSource::Inferred,
                inferred.map(|panels| {
                    panels
                        .into_iter()
                        .map(|panel| Cell {
                            target_id: panel.target_id as i32,
                            panel_id: format!("r{}c{}", panel.row, panel.column),
                            row: panel.row,
                            column: panel.column,
                        })
                        .collect()
                }),
            )
        }
    };
    let Some(mut cells) = cells else {
        // No Director record and no inferred grid: fall back to the
        // first user mosaic of this project, if one exists.
        let user = list_user_mosaics(conn, project_id)?;
        let Some(first) = user.into_iter().next() else {
            return Ok(None);
        };
        if first.panels.len() < 2 {
            return Ok(None);
        }
        return Ok(Some(ProjectMosaic {
            project_id,
            name: first.name,
            source: MosaicSource::User,
            rows: first.rows,
            columns: first.columns,
            panels: first.panels,
        }));
    };
    if cells.len() < 2 {
        return Ok(None);
    }
    cells.sort_by(|a, b| (a.row, a.column, &a.panel_id).cmp(&(b.row, b.column, &b.panel_id)));
    let rows = cells.iter().map(|cell| cell.row).max().unwrap_or(1);
    let columns = cells.iter().map(|cell| cell.column).max().unwrap_or(1);
    let panels = cells
        .into_iter()
        .map(|cell| MosaicPanel {
            target_id: cell.target_id,
            target_name: names.get(&cell.target_id).cloned().unwrap_or_default(),
            panel_id: cell.panel_id,
            row: cell.row,
            column: cell.column,
            preview: None,
        })
        .collect();
    Ok(Some(ProjectMosaic {
        project_id,
        name: project_name.unwrap_or_default().trim().to_owned(),
        source,
        rows,
        columns,
        panels,
    }))
}

fn target_names(conn: &Connection, project_id: i32) -> rusqlite::Result<HashMap<i32, String>> {
    let mut statement = conn.prepare("SELECT Id, name FROM target WHERE projectid=?1")?;
    let rows = statement.query_map([project_id], |row| {
        Ok((
            row.get::<_, i32>(0)?,
            row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        ))
    })?;
    rows.collect()
}

/// A target's place in the mosaic.
#[derive(Debug, Clone, PartialEq)]
struct Cell {
    target_id: i32,
    panel_id: String,
    row: u32,
    column: u32,
}

/// `r{row}c{column}`, one-based, as Director writes a grid's panel IDs.
fn parse_panel_id(id: &str) -> Option<(u32, u32)> {
    psf_guard_director_core::framing::layout::grid_cell(id)
}

/// The cells Director recorded when it activated this project, when it did:
/// every recorded target of one Director project, with the panel ID it
/// wrote. A grid's IDs are its cells; otherwise the targets' coordinates
/// place them. `None` when the database has no such record.
fn director_cells(conn: &Connection, project_id: i32) -> rusqlite::Result<Option<Vec<Cell>>> {
    let table: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='psf_guard_director_target'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if table.is_none() || conn.prepare("SELECT guid FROM target LIMIT 0").is_err() {
        return Ok(None);
    }
    let mut statement = conn.prepare(
        "SELECT t.Id, d.project_guid, d.panel_id
         FROM target t JOIN psf_guard_director_target d ON d.target_guid = t.guid
         WHERE t.projectid = ?1
         ORDER BY t.Id",
    )?;
    let rows: Vec<(i32, String, String)> = statement
        .query_map(params![project_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let Some((_, project_guid, _)) = rows.first() else {
        return Ok(None);
    };
    // One Director project per Target Scheduler project; a mix is not a
    // record to trust.
    if rows.iter().any(|(_, guid, _)| guid != project_guid) {
        return Ok(None);
    }
    // A mosaic has at least two panels the plan still holds: one panel and
    // the retired target it moved away from (`r1c1~3`) are a single panel.
    if rows
        .iter()
        .filter(|(_, _, panel_id)| !panel_id.contains('~'))
        .count()
        < 2
    {
        return Ok(None);
    }
    let grid: Option<Vec<Cell>> = rows
        .iter()
        .map(|(target_id, _, panel_id)| {
            parse_panel_id(panel_id).map(|(row, column)| Cell {
                target_id: *target_id,
                panel_id: panel_id.clone(),
                row,
                column,
            })
        })
        .collect();
    if let Some(grid) = grid {
        let mut seen = std::collections::HashSet::new();
        if grid.iter().all(|cell| seen.insert((cell.row, cell.column))) {
            return Ok(Some(grid));
        }
    }
    placed_cells(conn, &rows).map(Some)
}

/// Rows and columns for panels that form no full grid, from where their
/// targets point: each target's center on the plane at their mean, along
/// the first target's camera axes, banded by a quarter of the closest two
/// targets' spacing. Row 1 is the top and column 1 the camera's left (east
/// at angle zero). A cell two targets share puts the later one in a row of
/// its own below, so nothing hides.
fn placed_cells(conn: &Connection, rows: &[(i32, String, String)]) -> rusqlite::Result<Vec<Cell>> {
    use psf_guard_director_core::{framing::TangentPlane, visibility::IcrsPosition};
    let rotation = if conn.prepare("SELECT rotation FROM target LIMIT 0").is_ok() {
        "rotation"
    } else {
        "NULL"
    };
    let mut places = Vec::with_capacity(rows.len());
    for (target_id, _, panel_id) in rows {
        let (ra_hours, dec, angle): (Option<f64>, Option<f64>, Option<f64>) = conn.query_row(
            &format!("SELECT ra, dec, {rotation} FROM target WHERE Id=?1"),
            [target_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let center = ra_hours
            .zip(dec)
            .and_then(|(ra, dec)| crate::astrometry::target_scheduler_coordinates(ra, dec))
            .map(|(ra_degrees, dec_degrees)| IcrsPosition {
                ra_degrees,
                dec_degrees,
            });
        places.push((*target_id, panel_id.clone(), center, angle.unwrap_or(0.0)));
    }
    let located: Vec<_> = places.iter().filter_map(|p| p.2).collect();
    // The plane at the targets' mean, seen from the first target's plane.
    let offsets = located.first().and_then(|first| {
        let plane = TangentPlane::at(*first).ok()?;
        let projected: Option<Vec<[f64; 2]>> = located.iter().map(|c| plane.project(*c)).collect();
        let projected = projected?;
        let n = projected.len() as f64;
        let mean = [
            projected.iter().map(|o| o[0]).sum::<f64>() / n,
            projected.iter().map(|o| o[1]).sum::<f64>() / n,
        ];
        TangentPlane::at(plane.deproject(mean)).ok()
    });
    let angle = places.first().map_or(0.0, |p| p.3);
    let (sin, cos) = angle.to_radians().sin_cos();
    let camera: Vec<Option<(f64, f64)>> = places
        .iter()
        .map(|(_, _, center, _)| {
            let [xi, eta] = offsets.as_ref()?.project((*center)?)?;
            Some((-xi * cos + eta * sin, xi * sin + eta * cos))
        })
        .collect();
    let known: Vec<(f64, f64)> = camera.iter().flatten().copied().collect();
    let mut closest = f64::INFINITY;
    for (i, a) in known.iter().enumerate() {
        for b in &known[i + 1..] {
            let apart = (a.0 - b.0).hypot(a.1 - b.1);
            if apart > 1e-9 {
                closest = closest.min(apart);
            }
        }
    }
    let tolerance = if closest.is_finite() {
        closest / 4.0
    } else {
        1e-6
    };
    let band = |values: Vec<f64>| -> Vec<u32> {
        let mut order: Vec<usize> = (0..values.len()).collect();
        order.sort_by(|a, b| values[*a].total_cmp(&values[*b]));
        let mut out = vec![0; values.len()];
        let (mut current, mut last) = (0, None::<f64>);
        for index in order {
            if last.is_none_or(|last| values[index] - last > tolerance) {
                current += 1;
            }
            last = Some(values[index]);
            out[index] = current;
        }
        out
    };
    // A target with no coordinates sits after the others.
    let columns = band(camera.iter().map(|p| p.map_or(f64::MAX, |p| p.0)).collect());
    let rows_of = band(
        camera
            .iter()
            .map(|p| p.map_or(f64::MAX, |p| -p.1))
            .collect(),
    );
    let mut cells: Vec<Cell> = Vec::with_capacity(places.len());
    let mut extra = rows_of.iter().copied().max().unwrap_or(0);
    let mut taken = std::collections::HashSet::new();
    for (index, (target_id, panel_id, _, _)) in places.into_iter().enumerate() {
        let mut row = rows_of[index];
        if !taken.insert((row, columns[index])) {
            extra += 1;
            row = extra;
            taken.insert((row, columns[index]));
        }
        cells.push(Cell {
            target_id,
            panel_id,
            row,
            column: columns[index],
        });
    }
    Ok(cells)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::ts_schema::apply_schema(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO project (Id, profileId, name, state, guid) VALUES
                (1, 'p', 'Heart', 1, 'heart'),
                (2, 'p', 'M31', 1, 'm31'),
                (3, 'p', 'M81', 1, 'm81'),
                (4, 'p', 'Pairs', 1, 'pairs');",
        )
        .unwrap();
        conn
    }

    fn add_target(conn: &Connection, id: i32, project: i32, name: &str, ra_hours: f64, dec: f64) {
        conn.execute(
            "INSERT INTO target (Id, name, active, ra, dec, epochcode, rotation, roi, projectid, guid)
             VALUES (?1, ?2, 1, ?3, ?4, 2, 0, 100, ?5, ?6)",
            params![id, name, ra_hours, dec, project, format!("target-{id}")],
        )
        .unwrap();
    }

    fn cells(mosaic: &ProjectMosaic) -> Vec<(i32, &str)> {
        mosaic
            .panels
            .iter()
            .map(|panel| (panel.target_id, panel.panel_id.as_str()))
            .collect()
    }

    #[test]
    fn a_grid_of_targets_reads_as_its_mosaic_in_row_order() {
        // Askar107PHQ's Heart Mosaic as N.I.N.A. laid it out, stored out of
        // order and with names that say nothing about the cell.
        let conn = catalog();
        add_target(
            &conn,
            10,
            1,
            "Heart d",
            2.445_519_401_920_46,
            60.871_626_084_609_4,
        );
        add_target(
            &conn,
            11,
            1,
            "Heart a",
            2.645_499_681_489_58,
            61.832_231_223_272_4,
        );
        add_target(
            &conn,
            12,
            1,
            "Heart c",
            2.642_430_289_441_8,
            60.871_626_084_609_4,
        );
        add_target(
            &conn,
            13,
            1,
            "Heart b",
            2.442_450_009_872_69,
            61.832_231_223_272_4,
        );
        let mosaic = project_mosaic(&conn, 1).unwrap().expect("a 2×2 mosaic");
        assert_eq!(mosaic.source, MosaicSource::Inferred);
        assert_eq!((mosaic.rows, mosaic.columns), (2, 2));
        // East is left, so the larger right ascension is column 1.
        assert_eq!(
            cells(&mosaic),
            vec![(11, "r1c1"), (13, "r1c2"), (12, "r2c1"), (10, "r2c2")]
        );
        assert_eq!(mosaic.panels[0].target_name, "Heart a");
    }

    #[test]
    fn director_records_name_the_panels_ahead_of_the_coordinates() {
        let conn = catalog();
        // Coordinates that form no grid: only Director's record makes these
        // one mosaic.
        add_target(&conn, 20, 2, "M31 north", 0.70, 42.0);
        add_target(&conn, 21, 2, "M31 south", 0.75, 40.0);
        add_target(&conn, 22, 2, "M31 west", 0.60, 41.5);
        crate::server::director::create_rig_tables_for_tests(&conn);
        conn.execute_batch(
            "INSERT INTO psf_guard_director_target (target_guid, project_guid, panel_id, framing_revision) VALUES
                ('target-20', 'director-m31', 'r1c2', 3),
                ('target-21', 'director-m31', 'r2c1', 3),
                ('target-22', 'director-m31', 'r1c1', 3);",
        )
        .unwrap();
        let mosaic = project_mosaic(&conn, 2)
            .unwrap()
            .expect("Director's mosaic");
        assert_eq!(mosaic.source, MosaicSource::Director);
        assert_eq!((mosaic.rows, mosaic.columns), (2, 2));
        assert_eq!(
            cells(&mosaic),
            vec![(22, "r1c1"), (20, "r1c2"), (21, "r2c1")]
        );
    }

    #[test]
    fn director_panels_placed_one_by_one_read_as_one_mosaic() {
        // A framed target kept in place, one panel added east of it and one
        // north: an L, which no full grid holds.
        let conn = catalog();
        add_target(&conn, 50, 3, "M81", 9.9, 69.0);
        add_target(
            &conn,
            51,
            3,
            "M81 p2",
            9.9 + 1.6 / 15.0 / 69f64.to_radians().cos(),
            69.0,
        );
        add_target(&conn, 52, 3, "M81 p3", 9.9, 70.2);
        crate::server::director::create_rig_tables_for_tests(&conn);
        conn.execute_batch(
            "INSERT INTO psf_guard_director_target (target_guid, project_guid, panel_id, framing_revision) VALUES
                ('target-50', 'director-m81', 'r1c1', 1),
                ('target-51', 'director-m81', 'p2', 4),
                ('target-52', 'director-m81', 'p3', 4);",
        )
        .unwrap();
        let mosaic = project_mosaic(&conn, 3).unwrap().expect("one mosaic");
        assert_eq!(mosaic.source, MosaicSource::Director);
        assert_eq!((mosaic.rows, mosaic.columns), (2, 2));
        // North is the top row; east is the left column.
        assert_eq!(
            mosaic
                .panels
                .iter()
                .map(|p| (p.panel_id.as_str(), p.row, p.column))
                .collect::<Vec<_>>(),
            vec![("p3", 1, 2), ("p2", 2, 1), ("r1c1", 2, 2)]
        );
        assert_eq!(mosaic.panels[2].target_name, "M81");
    }

    #[test]
    fn a_panel_moved_off_its_old_target_keeps_both_in_the_mosaic() {
        let conn = catalog();
        add_target(&conn, 60, 2, "M31 r1c1", 0.70, 41.0);
        add_target(&conn, 61, 2, "M31 r1c2", 0.62, 41.0);
        add_target(&conn, 62, 2, "M31 r1c2", 0.54, 41.0);
        crate::server::director::create_rig_tables_for_tests(&conn);
        conn.execute_batch(
            "INSERT INTO psf_guard_director_target (target_guid, project_guid, panel_id, framing_revision) VALUES
                ('target-60', 'director-m31', 'r1c1', 2),
                ('target-61', 'director-m31', 'r1c2~2', 2),
                ('target-62', 'director-m31', 'r1c2', 3);",
        )
        .unwrap();
        let mosaic = project_mosaic(&conn, 2).unwrap().expect("one mosaic");
        assert_eq!(mosaic.source, MosaicSource::Director);
        assert_eq!(
            mosaic
                .panels
                .iter()
                .map(|p| (p.panel_id.as_str(), p.row, p.column))
                .collect::<Vec<_>>(),
            vec![("r1c1", 1, 1), ("r1c2~2", 1, 2), ("r1c2", 1, 3)]
        );
    }

    #[test]
    fn a_project_that_is_not_a_mosaic_has_none() {
        let conn = catalog();
        add_target(&conn, 30, 3, "M81", 9.9, 69.0);
        assert!(project_mosaic(&conn, 3).unwrap().is_none());
        // Names that read like panels are not a grid.
        add_target(&conn, 40, 4, "Pair r1c1", 5.0, 10.0);
        add_target(&conn, 41, 4, "Pair r1c2", 7.0, -20.0);
        add_target(&conn, 42, 4, "Pair r2c1", 12.0, 45.0);
        assert!(project_mosaic(&conn, 4).unwrap().is_none());
        // A project with no targets, and one that does not exist.
        assert!(project_mosaic(&conn, 99).unwrap().is_none());
    }

    #[test]
    fn panel_ids_parse_only_as_director_writes_them() {
        assert_eq!(parse_panel_id("r2c3"), Some((2, 3)));
        assert_eq!(parse_panel_id("r0c1"), None);
        assert_eq!(parse_panel_id("panel 1"), None);
        assert_eq!(parse_panel_id("r1"), None);
    }

    fn user_catalog() -> Connection {
        let conn = catalog();
        // LDN-shaped, in miniature: three mono panels that are three
        // corners of a 2x2 (no inferred mosaic), plus a wide target
        // that must stay out of the mono mosaics unless picked.
        add_target(&conn, 1, 1, "LDN 1163 Panel 1", 22.2258, 55.8143);
        add_target(&conn, 2, 1, "LDN 1163 Panel 2", 22.3461, 55.8227);
        add_target(&conn, 3, 1, "LDN 1163 Panel 0", 22.3338, 56.1835);
        add_target(&conn, 4, 1, "LDN 1163", 22.3057, 55.8547);
        conn.execute_batch(
            "INSERT INTO acquiredimage (Id, projectId, targetId, filtername, gradingStatus, metadata, profileId)
             VALUES
             (1, 1, 1, 'Ha', 1, '{\"FileName\":\"p1.fits\"}', 'p'),
             (2, 1, 2, 'Ha', 1, '{\"FileName\":\"p2.fits\"}', 'p'),
             (3, 1, 3, 'Ha', 1, '{\"FileName\":\"p0.fits\"}', 'p'),
             (4, 1, 4, 'SV220 Duo', 0, '{\"FileName\":\"osc.fits\"}', 'p');",
        )
        .unwrap();
        conn
    }

    fn create_request(
        name: &str,
        panels: Vec<(i32, Option<u32>, Option<u32>)>,
    ) -> CreateMosaicRequest {
        CreateMosaicRequest {
            name: name.to_owned(),
            panels: panels
                .into_iter()
                .map(|(target_id, row, column)| CreateMosaicPanel {
                    target_id,
                    row,
                    column,
                })
                .collect(),
        }
    }

    #[test]
    fn a_user_mosaic_holds_a_sparse_three_panel_grid_and_excludes_unpicked_targets() {
        let conn = user_catalog();
        assert!(project_mosaic(&conn, 1).unwrap().is_none());
        let mosaic = create_user_mosaic(
            &conn,
            1,
            &create_request(
                "Mono 3-panel",
                vec![
                    (3, Some(1), Some(2)),
                    (1, Some(2), Some(1)),
                    (2, Some(2), Some(2)),
                ],
            ),
        )
        .unwrap();
        assert_eq!(mosaic.source, MosaicSource::User);
        assert_eq!((mosaic.rows, mosaic.columns), (2, 2));
        assert_eq!(
            mosaic
                .panels
                .iter()
                .map(|panel| (panel.target_id, panel.panel_id.as_str()))
                .collect::<Vec<_>>(),
            vec![(3, "r1c2"), (1, "r2c1"), (2, "r2c2")]
        );
        assert!(
            !mosaic.panels.iter().any(|panel| panel.target_id == 4),
            "the wide target is not in a mono mosaic unless picked"
        );
        // The default mosaic falls back to the user mosaic here
        // because neither Director nor an inferred grid exists.
        let default = project_mosaic(&conn, 1).unwrap().expect("user fallback");
        assert_eq!(default.source, MosaicSource::User);
        assert_eq!(default.name, "Mono 3-panel");
        // And the named lookup finds it by id.
        let named = project_mosaic_named(&conn, 1, mosaic.id)
            .unwrap()
            .expect("named mosaic");
        assert_eq!(named.panels.len(), 3);
        assert!(project_mosaic_named(&conn, 1, mosaic.id + 99)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_two_panel_subset_and_auto_placement_work_and_validation_rejects_bad_picks() {
        let conn = user_catalog();
        let subset = create_user_mosaic(
            &conn,
            1,
            &create_request("Mono 1x2", vec![(1, None, None), (2, None, None)]),
        )
        .unwrap();
        assert_eq!(subset.panels.len(), 2);
        assert!(
            subset
                .panels
                .iter()
                .all(|panel| panel.row >= 1 && panel.column >= 1),
            "auto-placed panels get one-based rows and columns"
        );
        let mut cells: Vec<(u32, u32)> = subset
            .panels
            .iter()
            .map(|panel| (panel.row, panel.column))
            .collect();
        cells.sort_unstable();
        cells.dedup();
        assert_eq!(cells.len(), 2, "auto-placement must not share a cell");
        // Validation: one panel, a repeated target, a target of
        // another project, and a shared cell are all refused.
        assert!(
            create_user_mosaic(&conn, 1, &create_request("One", vec![(1, None, None)])).is_err()
        );
        assert!(create_user_mosaic(
            &conn,
            1,
            &create_request("Dup", vec![(1, None, None), (1, None, None)])
        )
        .is_err());
        assert!(create_user_mosaic(
            &conn,
            99,
            &create_request("Nope", vec![(1, None, None), (2, None, None)])
        )
        .is_err());
        assert!(create_user_mosaic(
            &conn,
            1,
            &create_request("Shared", vec![(1, Some(1), Some(1)), (2, Some(1), Some(1))])
        )
        .is_err());
        // Deleting a mosaic leaves every frame and target untouched.
        assert!(delete_user_mosaic(&conn, 1, subset.id).unwrap());
        assert!(list_user_mosaics(&conn, 1).unwrap().is_empty());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM acquiredimage", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            4
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM target WHERE projectid = 1",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            4
        );
    }

    #[test]
    fn a_suffixed_target_name_still_parses_as_its_panel_root() {
        // "LDN 1163 Panel 1 (2)" must keep the mosaic root "LDN 1163":
        // the suffix is a catalog index, not a new object name.
        let conn = user_catalog();
        conn.execute(
            "INSERT INTO target (Id, name, active, ra, dec, epochcode, rotation, roi, projectid, guid)
             VALUES (5, 'LDN 1163 Panel 1 (2)', 1, 22.2, 55.8, 2, 0, 100, 1, 'target-5')",
            [],
        )
        .unwrap();
        let mosaic = create_user_mosaic(
            &conn,
            1,
            &create_request(
                "With suffix",
                vec![(5, Some(2), Some(1)), (2, Some(2), Some(2))],
            ),
        )
        .unwrap();
        assert_eq!(mosaic.panels[0].target_name, "LDN 1163 Panel 1 (2)");
    }
}
