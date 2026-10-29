//! Team selectors are server-verified membership, never user authority claims.
use crate::{config, private_file, user_auth::UserClient};
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Role {
    Owner,
    Member,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Team {
    pub team_id: u64,
    pub name: String,
    pub my_role: Role,
    pub created_at: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedTeam {
    #[serde(flatten)]
    pub team: Team,
    pub invite_code: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinedTeam {
    pub team_id: u64,
    pub name: String,
    pub my_role: Role,
    pub joined_at: String,
    pub replayed: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub user_id: u64,
    pub name: String,
    pub nickname: Option<String>,
    pub role: Role,
    pub joined_at: String,
}

/// Cached selector only. require_selected always rechecks the current server user
/// and membership before a caller may use it for Agent bootstrap.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Selection {
    pub schema_version: u32,
    pub origin: String,
    pub user_id: u64,
    pub team_id: u64,
}

pub fn selection_path(config_path: &Path) -> PathBuf {
    config::state_directory(config_path).join("selected-team.json")
}

pub fn stored_selection(config_path: &Path) -> Result<Option<Selection>> {
    let value = private_file::read::<Selection>(&selection_path(config_path), 16_384)?;
    if let Some(s) = &value {
        ensure!(
            s.schema_version == 1 && valid_id(s.user_id) && valid_id(s.team_id),
            "Invalid selected team snapshot"
        );
    }
    Ok(value)
}

fn valid_id(id: u64) -> bool {
    id > 0 && id <= i64::MAX as u64
}
fn validate_team(team: &Team) -> Result<()> {
    ensure!(
        valid_id(team.team_id) && !team.name.is_empty(),
        "Invalid central team response"
    );
    Ok(())
}

async fn member_team(client: &mut UserClient, id: u64) -> Result<Team> {
    ensure!(valid_id(id), "Invalid team ID");
    let team: Team = client.get(&format!("/api/v1/teams/{id}")).await?;
    validate_team(&team)?;
    ensure!(team.team_id == id, "Central team identity mismatch");
    let members: Vec<Member> = client.get(&format!("/api/v1/teams/{id}/members")).await?;
    let user = client.user()?.user_id;
    ensure!(
        members.iter().all(|m| valid_id(m.user_id)),
        "Invalid central membership response"
    );
    ensure!(
        members
            .iter()
            .any(|m| m.user_id == user && m.role == team.my_role),
        "ACCESS_DENIED: verified current user is not a member of this team"
    );
    Ok(team)
}

pub async fn create(config_path: &Path, name: &str) -> Result<CreatedTeam> {
    ensure!(
        !name.trim().is_empty() && name.encode_utf16().count() <= 120,
        "Team name must contain 1 to 120 characters"
    );
    let mut client = UserClient::authenticated(config_path).await?;
    let created: CreatedTeam = client.post("/api/v1/teams", json!({"name":name})).await?;
    validate_team(&created.team)?;
    ensure!(
        created.team.my_role == Role::Owner
            && !created.invite_code.is_empty()
            && created.invite_code.len() <= 256,
        "Invalid central team creation response"
    );
    let verified=member_team(&mut client,created.team.team_id).await.map_err(|_|anyhow::anyhow!("Team {} was created but membership confirmation failed; check team list before retrying",created.team.team_id))?;
    ensure!(
        verified.my_role == Role::Owner,
        "Created team owner membership was not confirmed"
    );
    // Creation does not silently replace a selected team or an Agent binding.
    Ok(created)
}

pub async fn list(config_path: &Path) -> Result<Vec<Team>> {
    let mut client = UserClient::authenticated(config_path).await?;
    let teams: Vec<Team> = client.get("/api/v1/teams").await?;
    for team in &teams {
        validate_team(team)?;
    }
    Ok(teams)
}

pub async fn join(config_path: &Path, code: &str) -> Result<JoinedTeam> {
    ensure!(
        !code.is_empty() && code.len() <= 256,
        "Invalid invite code input"
    );
    let mut client = UserClient::authenticated(config_path).await?;
    let joined: JoinedTeam = client
        .post("/api/v1/teams/join", json!({"inviteCode":code}))
        .await?;
    ensure!(valid_id(joined.team_id), "Invalid joined team response");
    let verified = member_team(&mut client, joined.team_id).await?;
    ensure!(
        verified.my_role == joined.my_role,
        "Joined team membership mismatch"
    );
    Ok(joined)
}

pub async fn members(config_path: &Path, id: u64) -> Result<Vec<Member>> {
    ensure!(valid_id(id), "Invalid team ID");
    let mut client = UserClient::authenticated(config_path).await?;
    let members: Vec<Member> = client.get(&format!("/api/v1/teams/{id}/members")).await?;
    ensure!(
        members.iter().all(|m| valid_id(m.user_id)),
        "Invalid central membership response"
    );
    Ok(members)
}

pub async fn select(config_path: &Path, id: u64) -> Result<Team> {
    let mut client = UserClient::authenticated(config_path).await?;
    let team = member_team(&mut client, id).await?;
    let selection = Selection {
        schema_version: 1,
        origin: client.central_origin().to_owned(),
        user_id: client.user()?.user_id,
        team_id: team.team_id,
    };
    private_file::write(&selection_path(config_path), &selection, 16_384)?;
    Ok(team)
}

pub async fn require_selected(config_path: &Path, client: &mut UserClient) -> Result<Team> {
    let Some(selection) = stored_selection(config_path)? else {
        bail!("TEAM_SELECTION_REQUIRED: run devlink team select --id <ID>");
    };
    ensure!(
        selection.origin == client.central_origin() && selection.user_id == client.user()?.user_id,
        "TEAM_SELECTION_REQUIRED: select a team for the current central user"
    );
    member_team(client, selection.team_id).await
}

pub async fn current(config_path: &Path) -> Result<Option<Team>> {
    let mut client = UserClient::authenticated(config_path).await?;
    if stored_selection(config_path)?.is_none() {
        return Ok(None);
    }
    require_selected(config_path, &mut client).await.map(Some)
}
