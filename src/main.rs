use std::{
	process,
	error::{
		Error
	},
	fs::{
		write,
		create_dir_all,
		Metadata,
		read_to_string,
		File as StdFile
	},
	io::{
		ErrorKind,
		stdout
	},
	path::{
		Path,
		PathBuf
	},
	time::{
		SystemTime,
		UNIX_EPOCH
	},
	env::{
		temp_dir,
		current_dir
	}
};

use crossterm::{
	cursor,
	event::{
		self,
		Event
	},
	terminal::{
		self,
		ClearType::{
			All
		}
	},
	ExecutableCommand
};

use serde::{
	Serialize,
	Deserialize
};

use clap::{
	Parser,
	Subcommand,
	CommandFactory,
};

use clap_complete::{
	Shell
};

use futures_util::{
	StreamExt
};

use indicatif::{
	ProgressBar,
	ProgressStyle
};

use inquire::{
	Password,
	PasswordDisplayMode::{
		Masked
	},
	Select
};

use qrcode::{
	QrCode,
	render::{
		unicode
	},
};

use reqwest::{
	Client,
	header,
	multipart::{
		Form,
		Part
	}
};

use tokio::{
	fs::{
		File,
		metadata,
		rename,
		copy,
		remove_file
	},
	io::{
		AsyncWriteExt
	}
};

use tokio_util::{
	io::{
		ReaderStream
	}
};

use bip39::{
	Mnemonic,
	Language::{
		English
	}
};

use age::{
	Encryptor,
	Decryptor,
	secrecy::{
		SecretString
	},
	scrypt::{
		Identity
	}
};

const HISTORY_MAX: usize = 15;

fn wait_and_clear_screen() {
	println!( "Press any key to clear screen." );
	let _ = terminal::enable_raw_mode();
	loop {
		if let Ok( Event::Key( _ ) ) = event::read() { break; }
	}
	let _ = terminal::disable_raw_mode();
	let _ = stdout().execute( terminal::Clear( All ) );
	let _ = stdout().execute( cursor::MoveTo( 0, 0 ) );
}
const DEFAULT_SERVER: &str = "https://send.withcapsule.dev";

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum HistoryEntry {
	Upload {
		file_name: String,
		file_id:   String,
		url:       String,
		encrypted: bool,
		timestamp: u64,
	},
	Download {
		file_name: String,
		file_id:   String,
		encrypted: bool,
		timestamp: u64,
	},
}

fn history_path() -> Option<PathBuf> {
	let mut path = dirs::data_dir()?;
	path.push( "capsule" );
	path.push( "history.json" );
	Some( path )
}

fn server_config_path() -> Option<PathBuf> {
	let mut path = dirs::data_dir()?;

	path.push( "capsule" );
	path.push( "server.txt" );

	return Some( path );
}

fn load_server() -> String {
	let path = match server_config_path() {
		Some( p ) => p,
		None => return DEFAULT_SERVER.to_string(),
	};

	read_to_string( &path ).ok().map( |s| s.trim().to_string() ).filter( |s| !s.is_empty() ).unwrap_or_else( || DEFAULT_SERVER.to_string() )
}

fn save_server( server: &str ) {
	let path = match server_config_path() {
		Some( p ) => p,
		None => return,
	};

	if let Some( parent ) = path.parent() {
		let _ = create_dir_all( parent );
	}

	let _ = write( &path, server );
}

fn load_history() -> Vec<HistoryEntry> {
	let path = match history_path() {
		Some( p ) => p,
		None => return vec![],
	};
	let data = match std::fs::read_to_string( &path ) {
		Ok( s ) => s,
		Err( _ ) => return vec![],
	};
	serde_json::from_str( &data ).unwrap_or_default()
}

fn save_history( mut entries: Vec<HistoryEntry> ) {
	let path = match history_path() {
		Some( p ) => p,
		None => return,
	};
	if let Some( parent ) = path.parent() {
		let _ = std::fs::create_dir_all( parent );
	}
	if entries.len() > HISTORY_MAX {
		entries.drain( 0..entries.len() - HISTORY_MAX );
	}
	if let Ok( json ) = serde_json::to_string_pretty( &entries ) {
		let _ = std::fs::write( &path, json );
	}
}

fn record_upload( file_name: String, file_id: String, url: String, encrypted: bool ) {
	let mut entries = load_history();
	entries.push( HistoryEntry::Upload {
		file_name,
		file_id,
		url,
		encrypted,
		timestamp: SystemTime::now().duration_since( UNIX_EPOCH ).map( |d| d.as_secs() ).unwrap_or( 0 ),
	} );
	save_history( entries );
}

fn record_download( file_name: String, file_id: String, encrypted: bool ) {
	let mut entries = load_history();
	entries.push( HistoryEntry::Download {
		file_name,
		file_id,
		encrypted,
		timestamp: SystemTime::now().duration_since( UNIX_EPOCH ).map( |d| d.as_secs() ).unwrap_or( 0 ),
	} );
	save_history( entries );
}

fn format_timestamp( secs: u64 ) -> String {
	let now = SystemTime::now().duration_since( UNIX_EPOCH ).map( |d| d.as_secs() ).unwrap_or( 0 );
	let diff = now.saturating_sub( secs );

	match diff {
		0..=59           => "just now".to_string(),
		60..=3599        => format!( "{}m ago", diff / 60 ),
		3600..=86399     => format!( "{}h ago", diff / 3600 ),
		86400..=2591999  => format!( "{}d ago", diff / 86400 ),
		_                => format!( "{}mo ago", diff / 2592000 ),
	}
}

#[derive(Parser)]
#[command(name = "capsule", version, about = "CLI for the Capsule server")]
struct CLI {
	#[arg(long)]
	server: Option<String>,

	#[command(subcommand)]
	command: Command,
}


#[derive(Subcommand)]
enum RecentsCommand {
	#[command(about = "Clear all recent history")]
	Clear,
}

#[derive(Subcommand)]
enum ServerCommand {
	#[command(about = "Show the currently active server")]
	Info,

	#[command(about = "Set a custom server address")]
	Set {
		address: String,
	},

	#[command(about = "Reset the server to the default (https://send.withcapsule.dev)")]
	Reset,
}

#[derive(Subcommand)]
enum Command {
	#[command(visible_alias = "p", about = "Test server connection with a ping")]
	Ping,

	#[command(visible_alias = "u", about = "Upload a file to the server")]
	Upload {
		path: PathBuf
	},

	#[command(visible_alias = "ue", about = "Locally encrypt a file, then upload a file to the server")]
	UploadEncrypted {
		path: PathBuf
	},

	#[command(visible_alias = "d", about = "Download a recently uploaded file")]
	Download {
		id_or_url: String,

		#[arg(short, long)]
		output: Option<PathBuf>,
	},

	#[command(visible_alias = "s", about = "Show status and metadata for an uploaded file")]
	Status {
		id_or_url: String
	},

	#[command(visible_alias = "r", about = "Show recent uploads and downloads, --help for more")]
	Recents {
		#[command(subcommand)]
		action: Option<RecentsCommand>,
	},

	#[command( visible_alias = "del", about = "Delete a file by ID" )]
	Delete {
		id_or_url: String
	},

	#[command(visible_alias = "sv", about = "Manage the server address")]
	Server {
		#[command(subcommand)]
		action: ServerCommand,
	},

	#[command(hide = true, about = "Generate shell completions")]
	Completions {
		#[arg(value_enum)]
		shell: Shell,
	},
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
	let cli = CLI::parse();

	if let Command::Completions { shell } = cli.command {
		clap_complete::generate( shell, &mut CLI::command(), "capsule", &mut std::io::stdout() );
		return Ok( () );
	}

	let base = cli.server
		.unwrap_or_else( load_server )
		.trim_end_matches( '/' )
		.to_string();
	
    let base = &base;
	
    let client = Client::new();

	match cli.command {
		Command::Completions { .. } => unreachable!(),

		Command::Server { action } => {
			match action {
				ServerCommand::Info => {
					let server = load_server();
					let is_default = server == DEFAULT_SERVER;
					println!( "\n  Server: {}{}\n",
						highlight_link( &server ),
						if is_default { "  (default)" } else { "" }
					);
				}
				ServerCommand::Set { address } => {
					let address = address.trim_end_matches( '/' ).to_string();
					save_server( &address );
					println!( "\n  Server set to: {}\n", highlight_link( &address ) );
				}
				ServerCommand::Reset => {
					save_server( DEFAULT_SERVER );
					println!( "\n  Server reset to: {}\n", highlight_link( DEFAULT_SERVER ) );
				}
			}
			return Ok( () );
		}

		Command::Ping => {
			let url = format!( "{}/ping", base );
			let resp = client.get( url ).send().await?;
			println!( "{}", resp.text().await? );
		}

		Command::Upload { path } => {
			upload_file( &client, base, path, false ).await?;
		}

		Command::UploadEncrypted { path } => {
			upload_file( &client, base, path, true ).await?;
		}

		Command::Download { id_or_url, output } => {
			download_file( &client, base, id_or_url, output ).await?;
		}

		Command::Status { id_or_url } => {
			file_status( &client, base, id_or_url ).await?;
		}

		Command::Recents { action } => {
			match action {
				Some( RecentsCommand::Clear ) => {
					save_history( vec![] );
					println!( "\nHistory cleared.\n" );
				}
				None => {
					let entries = load_history();

					if entries.is_empty() {
						println!( "\nNo recent activity.\n" );
						return Ok( () );
					}

					println!();

					for entry in entries.iter().rev() {
						match entry {
							HistoryEntry::Upload { file_name, file_id, url, encrypted, timestamp } => {
								let lock = if *encrypted { " [encrypted]" } else { "" };
								println!(
									"  \x1b[92mUPLOAD\x1b[0m  {}{}\n          ID: {}  |  {}\n          {}\n",
									file_name, lock,
									highlight_id( file_id ),
									format_timestamp( *timestamp ),
									highlight_link( url ),
								);
							}
							HistoryEntry::Download { file_name, file_id, encrypted, timestamp } => {
								let lock = if *encrypted { " [encrypted]" } else { "" };
								println!(
									"  \x1b[94mDOWNLOAD\x1b[0m  {}{}\n            ID: {}  |  {}\n",
									file_name, lock,
									highlight_id( file_id ),
									format_timestamp( *timestamp ),
								);
							}
						}
					}
				}
			}
		}

		Command::Delete { id_or_url } => {
			let id: String = extract_id_from_input( &id_or_url );
			let res = client.delete( format!( "{}/delete/{}", base, id ) ).send().await?;

			if !res.status().is_success() {
				let status = res.status();
				let body = res.text().await.unwrap_or_default();
				eprintln!( "\nDeletion failed: ({}): {}\n", status, body );
				return Ok( () )
			}

			println!( "\nFile {} deleted", id );
		}
	}

	return Ok( () );
}

fn extract_file_id( body: &str ) -> Option<String> {
	let marker = "File ID for downloading is ";
	let start = body.find( marker )?;
	let after = &body[ start + marker.len().. ];

	let id: String = after.chars().take_while( |c| c.is_ascii_alphanumeric() ).collect();

	if id.is_empty() { None } else { Some( id ) }
}

fn extract_id_from_input( input: &str ) -> String {
	if let Some( idx ) = input.rfind( "/download/" ) {
		let after = &input[ idx + "/download/".len().. ];
		let trimmed = after.trim_matches( '/' );

		if trimmed.is_empty() { input.to_string() } else { trimmed.to_string() }
	} else {
		input.to_string()
	}
}

fn filename_from_headers( headers: &header::HeaderMap ) -> Option<PathBuf> {
	let value = headers.get( header::CONTENT_DISPOSITION )?.to_str().ok()?;
	let filename = parse_filename_param( value )?;
	if filename.is_empty() { None } else { Some( Path::new( &filename ).file_name()?.into() ) }
}

fn parse_filename_param( header_value: &str ) -> Option<String> {
	for part in header_value.split( ';' ) {
		let trimmed = part.trim();

		if let Some( rest ) = trimmed.strip_prefix( "filename=" ) {
			return Some( rest.trim_matches( '"' ).to_string() );
		}

		if let Some( rest ) = trimmed.strip_prefix( "filename*=" ) {
			let decoded = rest.trim_matches( '"' );

			if let Some( idx ) = decoded.find( "''" ) {
				return Some( decoded[ idx + 2.. ].to_string() );
			}

			return Some( decoded.to_string() );
		}
	}

	return None;
}

fn highlight_path( path: &Path ) -> String {
	format!( "\x1b[94m{}\x1b[0m", path.display() )
}

fn highlight_link( value: &str ) -> String {
	format!( "\x1b[92m{}\x1b[0m", value )
}

fn highlight_id( value: &str ) -> String {
	format!( "\x1b[92m{}\x1b[0m", value )
}

fn format_size( bytes: u64 ) -> String {
	match bytes {
		b if b < 1024             => format!( "{} B", b ),
		b if b < 1024 * 1024      => format!( "{:.1} KB", b as f64 / 1024.0 ),
		b if b < 1024 * 1024 * 1024 => format!( "{:.1} MB", b as f64 / (1024.0 * 1024.0) ),
		b                         => format!( "{:.2} GB", b as f64 / (1024.0 * 1024.0 * 1024.0) ),
	}
}

fn format_duration( secs: u64 ) -> String {
	match secs {
		0              => "expired".to_string(),
		1..=59         => format!( "{}s", secs ),
		60..=3599      => format!( "{}m {}s", secs / 60, secs % 60 ),
		_              => format!( "{}h {}m", secs / 3600, (secs % 3600) / 60 ),
	}
}

async fn file_status( client: &Client, base: &str, id_or_url: String ) -> Result<(), Box<dyn Error>> {
	let id = extract_id_from_input( &id_or_url );
	let url = format!( "{}/status/{}", base, id );
	let resp = client.get( &url ).send().await?;

	if !resp.status().is_success() {
		let status = resp.status();
		let body = resp.text().await.unwrap_or_default();
		eprintln!( "\nStatus failed ({}): {}\n", status, body );
		return Ok( () );
	}

	let json: serde_json::Value = resp.json().await?;

	let file_name    = json["file_name"].as_str().unwrap_or( "unknown" );
	let file_size    = json["file_size"].as_u64().unwrap_or( 0 );
	let upload_time  = json["upload_time"].as_u64().unwrap_or( 0 );
	let time_remaining = json["time_remaining"].as_u64().unwrap_or( 0 );
	let is_encrypted = json["is_encrypted"].as_bool().unwrap_or( false );

	let lock = if is_encrypted { " \x1b[93m[encrypted]\x1b[0m" } else { "" };

	println!( "\n  \x1b[92m{}\x1b[0m{}", file_name, lock );
	println!( "  Size:       {}", format_size( file_size ) );
	println!( "  Uploaded:   {}", format_timestamp( upload_time ) );
	println!( "  Expires in: {}", format_duration( time_remaining ) );
	println!( "  ID:         {}\n", highlight_id( &id ) );

	return Ok( () );
}

fn encrypt_into_temp_file( path: &Path, passphrase: SecretString, file_size: u64 ) -> Result<PathBuf, Box<dyn Error>> {
	let mut temp_path: PathBuf = temp_dir();
	temp_path.push(
		format!( "capsule-{}-{}",
			SystemTime::now().duration_since( UNIX_EPOCH )?.as_nanos(),
			process::id(),
		)
	);

	let input_file = StdFile::open( path )?;
	let output_file = StdFile::create( temp_path.clone() )?;

	let pb = ProgressBar::new( file_size );
	pb.set_style( ProgressStyle::default_bar()
		.template( "Encrypting {spinner:.green} [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})" )?
		.progress_chars( "#>-" )
	);

	let encryptor: Encryptor = Encryptor::with_user_passphrase( passphrase );
	let mut file_writer = encryptor.wrap_output( output_file )?;
	let mut file_reader = std::io::BufReader::new( pb.wrap_read( input_file ) );

	std::io::copy( &mut file_reader, &mut file_writer )?;
	file_writer.finish()?;

	pb.finish_with_message( "Encryption complete" );

	return Ok( temp_path );
}

fn decrypt_from_temp_file( temp_path: &Path, output_path: &Path, passphrase: SecretString ) -> Result<(), Box<dyn Error>> {
	let input_file = StdFile::open( temp_path )?;
	let file_size = input_file.metadata()?.len();

	let pb = ProgressBar::new( file_size );
	pb.set_style( ProgressStyle::default_bar()
		.template( "Decrypting {spinner:.green} [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})" )?
		.progress_chars( "#>-" )
	);

	let decryptor = Decryptor::new( pb.wrap_read( input_file ) )?;
	let identity = Identity::new( passphrase );
	let mut decrypted = decryptor.decrypt( std::iter::once( &identity as &dyn age::Identity ) )?;
	let mut output_file = StdFile::create( output_path )?;

	std::io::copy( &mut decrypted, &mut output_file )?;

	pb.finish_with_message( "Decryption complete" );

	return Ok( () );
}

async fn upload_file( client:&Client, base: &str, path: PathBuf, encrypt: bool ) -> Result<(), Box<dyn Error>> {
	let url = if encrypt {
		format!( "{}/upload?encrypted=true", base )
	} else {
		format!( "{}/upload", base )
	};

	let file_metadata: Metadata = match metadata( &path ).await {
		Ok( metadata ) => metadata,

		Err( err ) if err.kind() == ErrorKind::NotFound => {
			eprintln!( "\nUpload aborted: file {} not found\n", highlight_path( &path ) );
			return Ok( () );
		}

		Err( err ) => return Err( err.into() ),
	};

	if file_metadata.is_dir() {
		eprintln!( "\nUpload aborted: {} is a directory. Please provide a file path.\n", highlight_path( &path ) );
		return Ok( () );
	}

	let file_size: u64 = file_metadata.len();

	let pb = ProgressBar::new( file_size );
	pb.set_style( ProgressStyle::default_bar()
		.template( "Uploading {spinner:.green} [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})" )?
		.progress_chars( "#>-" )
	);

	let mut upload_source_file: Option<PathBuf> = None;
	let mut upload_temp_file: Option<PathBuf> = None;
	let mut upload_decryption_key: Option<String> = None;

	let mnemonic: Mnemonic;
	let mnemonic_text: String;
	let passphrase: SecretString;
	let encrypted_file_path: PathBuf;

	if encrypt {
		mnemonic = Mnemonic::generate_in( English, 12 )?;
		mnemonic_text = mnemonic.to_string();

		passphrase = SecretString::new( mnemonic_text.clone().into() );
		encrypted_file_path = encrypt_into_temp_file( &path, passphrase, file_size )?;

		upload_source_file = Some( encrypted_file_path.clone() );
		upload_temp_file = Some( encrypted_file_path );
		upload_decryption_key = Some( mnemonic_text );
	}


	let upload_path = upload_source_file.as_deref().unwrap_or( &path );

	let upload_size = if upload_source_file.is_some() {
		metadata( upload_path ).await?.len()
	} else {
		file_size
	};

	pb.set_length( upload_size );

	let file = File::open( upload_path ).await?;
	let file_name = path.file_name()
		.map( |n| n.to_string_lossy().to_string() )
		.unwrap_or_else( || "file".to_string() );

	let pb_clone = pb.clone();
	let stream = ReaderStream::new( file ).map( move |chunk| {
		chunk.map( |bytes| {
			pb_clone.inc( bytes.len() as u64 );
			bytes
		} )
	} );

	let body = reqwest::Body::wrap_stream( stream );
	let part = Part::stream_with_length( body, upload_size ).file_name( file_name );
	let form = Form::new().part( "f", part );

	let resp = client.post( url ).multipart( form ).send().await?;
	pb.finish_with_message( "Upload complete" );

	if let Some( temp ) = upload_temp_file {
		let _ = tokio::fs::remove_file( temp ).await;
	}

	let body = resp.text().await?;

	if let Some( file_id ) = extract_file_id( &body ) {
		let download_url = format!( "{}/download/{}", base, file_id );
		println!( "\n\nDownload Link: {}", highlight_link( &download_url ) );
		println!( "File ID: {}\n", highlight_id( &file_id ) );

		record_upload(
			path.file_name().map( |n| n.to_string_lossy().to_string() ).unwrap_or_default(),
			file_id.clone(),
			download_url.clone(),
			encrypt,
		);

		let qr_sel = Select::new( "Show download link as QR code?", vec![ "Show QR code", "Continue" ] )
			.with_vim_mode( true )
			.prompt();

		if let Ok( choice ) = qr_sel {
			if choice == "Show QR code" {
				if let Ok( code ) = QrCode::new( &download_url ) {
					let image = code.render::<unicode::Dense1x2>()
						.quiet_zone( true )
						.module_dimensions( 1, 1 )
						.build();
					println!( "\n{}\n", image );
				}
			}
		}

		if encrypt {
			let phrase_sel = Select::new( "View Decryption Phrase as:", vec![ "As plain text", "Plain text and QR code" ] )
				.with_vim_mode( true )
				.prompt();

			if let Ok( choice ) = phrase_sel {
				if let Some( ref key ) = upload_decryption_key {
					println!( "\nDecryption Phrases: {}\n", key );
					if choice == "Plain text and QR code" {
						if let Ok( code ) = QrCode::new( key.as_str() ) {
							let image = code.render::<unicode::Dense1x2>()
								.quiet_zone( true )
								.module_dimensions( 1, 1 )
								.build();
							println!( "{}\n", image );
						}
					}
					wait_and_clear_screen();
				}
			}
		}
	} else { println!( "\n{}", body.trim_end() ); }

	return Ok( () );
}

async fn download_file( client:&Client, base: &str, id_or_url: String, output: Option<PathBuf> ) -> Result<(), Box<dyn Error>> {
	let id = extract_id_from_input( &id_or_url );
	let url = format!( "{}/download/{}", base, id );
	let resp = client.get( url ).send().await?;

	if !resp.status().is_success() {
		let status = resp.status();
		let body = resp.text().await.unwrap_or_default();

		return Err( format!( "Download failed ({}): {}", status, body ).into() );
	}

	let is_encrypted = resp.headers()
		.get( "X-Encrypted" )
		.and_then( |v| v.to_str().ok() )
		.map( |v| v.eq_ignore_ascii_case( "true" ) )
		.unwrap_or( false );

	let total_size = resp.content_length().unwrap_or( 0 );
	let pb = ProgressBar::new( total_size );
    pb.set_style( ProgressStyle::default_bar()
		.template( "{spinner:.green} [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})" )?
		.progress_chars( "#>-" )
	);

	let filename = output.unwrap_or_else( || {
		filename_from_headers( resp.headers() ).unwrap_or_else( || PathBuf::from( "download" ) )
	} );

	match tokio::fs::metadata( &filename ).await {
		Ok( _ ) => {
			let options = vec![ "No", "Yes" ];
			let selection = Select::new( &format!( "File `{}` already exists. Overwrite?", {filename.to_string_lossy().to_string()} ).to_string(), options ).with_vim_mode( true ).prompt();

			match selection {
				Ok( choice ) if choice == "Yes" => {}
				_ => {
					println!( "\nDownload cancelled.\n" );
					return Ok( () );
				}
			}
		}
		Err( err ) if err.kind() == ErrorKind::NotFound => {}
		Err( err ) => return Err( err.into() ),
	}

	let download_path = if is_encrypted {
		let mut temp = temp_dir();
		temp.push( format!( "capsule-dl-{}-{}", SystemTime::now().duration_since( UNIX_EPOCH )?.as_nanos(), process::id() ) );
		temp
	} else {
		filename.clone()
	};

	let mut file = File::create( &download_path ).await?;
	let mut stream = resp.bytes_stream();

	while let Some( chunk ) = stream.next().await {
		let bytes = chunk?;
		pb.inc( bytes.len() as u64 );
		file.write_all( &bytes ).await?;
	}

	pb.finish_with_message( "Download complete" );

	if is_encrypted {
		const MAX_ATTEMPTS: u32 = 3;
		let mut attempts = 0;

		loop {
			let passphrase_input = Password::new( "Decryption mnemonic phrases:" )
				.without_confirmation()
				.with_display_mode( Masked )
				.prompt()?;

			let passphrase = SecretString::new( passphrase_input.into() );

			match decrypt_from_temp_file( &download_path, &filename, passphrase ) {
				Ok( _ ) => {
					let _ = remove_file( &download_path ).await;
					break;
				}

				Err( _ ) => {
					attempts += 1;
					let remaining = MAX_ATTEMPTS - attempts;

					if remaining > 0 {
						eprintln!( "\nDecryption failed. {} attempt{} remaining.\n", remaining, if remaining == 1 { "" } else { "s" } );
						continue;
					}

					eprintln!( "\nDecryption failed after {} attempts.\n", MAX_ATTEMPTS );

					let choice = Select::new( "What would you like to do?", vec![
						"Exit (delete downloaded file)",
						"Keep file and exit",
						"Try again",
					] ).with_vim_mode( true ).prompt();

					match choice {
						Ok( "Keep file and exit" ) => {
							let dest = current_dir()?.join( filename.file_name().unwrap_or( filename.as_os_str() ) );
							if rename( &download_path, &dest ).await.is_err() {
								copy( &download_path, &dest ).await?;
								let _ = remove_file( &download_path ).await;
							}
							println!( "\nEncrypted file kept at: {}\n", dest.display() );
							return Ok( () );
						}
						Ok( "Try again" ) => {
							attempts = 0;
							continue;
						}
						_ => {
							let _ = remove_file( &download_path ).await;
							println!( "\nDownload discarded.\n" );
							return Ok( () );
						}
					}
				}
			}
		}
	}

	record_download(
		filename.file_name().map( |n| n.to_string_lossy().to_string() ).unwrap_or_default(),
		id,
		is_encrypted,
	);

	println!( "\nSaved to: {}\n", filename.display() );

	return Ok( () )
}

