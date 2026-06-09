

// ============================================================================
// MCP Server Demo — 1C:Enterprise External Data Processor
// ============================================================================
//
// This module demonstrates how to use the http1c native component to build
// a fully-featured MCP (Model Context Protocol) server from 1C:Enterprise.
//
// Architecture:
//   Native DLL (HttpServer)  <->  1C BSL (this module)  <->  MCP Client (VS Code)
//
// The native component handles:
//   - HTTP transport (Streamable HTTP on localhost)
//   - JSON-RPC 2.0 message framing
//   - MCP session management (Mcp-Session-Id)
//   - Security (Origin validation, Bearer auth, rate limiting)
//   - SSE streaming for progress notifications
//
// This 1C module handles:
//   - Registering MCP tools, resources, and prompts
//   - Processing tool calls, resource reads, and prompt gets
//   - Business logic execution in the 1C context
//
// Usage as a Constructor Pattern:
//   1. Create an instance of the component
//   2. Register tools with RegisterTools() - JSON array of tool definitions
//   3. Register resources with RegisterResources() - JSON array of resource defs
//   4. Register prompts with RegisterPrompts() - JSON array of prompt defs
//   5. Start listening with StartListen(port)
//   6. Handle ExternalEvents: ToolCall, ResourceRead, PromptGet
//   7. Send results back with SendResponse()
//
// Each tool/resource/prompt can be added independently. The component
// supports dynamic registration - call RegisterTools() again at any time
// with an updated list (the MCP client will be notified via listChanged).
//
// ============================================================================

#Region Variables

&AtClient
Var Component;

&AtClient
Var AddInPath;

&AtClient
Var RuntimeStatus;

&AtClient
Var DefaultLogPath;

&AtClient
Var ScreenshotDataArray;

&AtClient
Var ScreenshotCurrentIndex;

&AtClient
Var SelfTestCtx;

&AtClient
Var SelfTestWaitTicks;

&AtClient
Var SelfTestCfg;

&AtClient
Var EmbedCtx;

&AtClient
Var SyncCtx;

#EndRegion


#Region FormEventHandlers

// ---------------------------------------------------------------------------
// ExternalEvent - main entry point for all events from the native component.
//
// The native DLL sends events via ExternalEvent mechanism:
//   Source = "HttpServer"
//   Event  = "ToolCall"     - MCP tools/call request
//          | "ResourceRead" - MCP resources/read request
//          | "PromptGet"    - MCP prompts/get request
//          | "Request"      - Legacy HTTP request (non-MCP)
//   Data   = JSON string with request details
// ---------------------------------------------------------------------------
&AtClient
Procedure ExternalEvent(Source, Event, Data)
	
	If Source <> "HttpServer" Then
		Return;
	EndIf;
	
	If Event = "ToolCall" Then
		ProcessToolCall(Data);
	ElsIf Event = "ResourceRead" Then
		ProcessResourceRead(Data);
	ElsIf Event = "PromptGet" Then
		ProcessPromptGet(Data);
	ElsIf Event = "Request" Then
		ProcessLegacyRequest(Data);
	Else
		Return;
	EndIf;

EndProcedure

// ---------------------------------------------------------------------------
// OnOpen — prefill the RAG demo fields with sensible defaults.
// ---------------------------------------------------------------------------
&AtClient
Procedure OnOpen(Cancel)

	SelfTestCfg = ParseLaunchConfig();

	// Trace that OnOpen fired + capture the launch parameter (best-effort).
	Try
		W = New TextWriter(SelfTestOutFile("onopen-trace.txt"), TextEncoding.UTF8);
		W.Write("OnOpen fired " + String(CurrentDate())
			+ " build=" + BuildVersion() + Chars.LF
			+ "LaunchParameter=[" + String(LaunchParameter) + "]");
		W.Close();
	Except
	EndTry;

	EnsureRagDefaults();

	// Attach the component on open (attach-only — no InstallAddIn modal). Shipping
	// path attaches the declared template bundle (lite). The headless self-test
	// instead attaches the on-disk DLL in ExtCompT IN-PLACE so rcore.dll is found
	// beside libhttp1cWin.dll (real search) — its location comes from the launch
	// config, never hardcoded.
	If SelfTestCfg.selftest Then
		AttachSelfTestComponent(0);
	Else
		AddInPath = GetDefaultAddInSource();
		BeginAttachingAddIn(New NotifyDescription("OnOpenAttachEnd", ThisObject),
			AddInPath, "http1c", AddInType.Native);
	EndIf;

EndProcedure

// Parse "ragselftest;model=<path>;out=<dir>;extcompt=<dllpath>" from the launch
// parameter so every environment-specific absolute path stays OUT of the
// processor — the launcher (dev tooling) supplies them.
&AtClient
Function ParseLaunchConfig()
	Cfg = New Structure("selftest, model, out, extcompt, perf, embedperf, batch, workers, synctest, steps, corpus, coll",
		False, "", "", "", 0, "", 500, 1, False, "", "", "");
	LP = "";
	Try
		LP = String(LaunchParameter);
	Except
	EndTry;
	For Each Part In StrSplit(LP, ";", False) Do
		Part = TrimAll(Part);
		If Lower(Part) = "ragselftest" Then
			Cfg.selftest = True;
		ElsIf StrStartsWith(Part, "model=") Then
			Cfg.model = Mid(Part, StrLen("model=") + 1);
		ElsIf StrStartsWith(Part, "out=") Then
			Cfg.out = Mid(Part, StrLen("out=") + 1);
		ElsIf StrStartsWith(Part, "extcompt=") Then
			Cfg.extcompt = Mid(Part, StrLen("extcompt=") + 1);
		ElsIf StrStartsWith(Part, "perf=") Then
			// perf=N → run the keyword-latency benchmark over an N-segment synthetic
			// corpus instead of the normal assert contour.
			Try
				Cfg.perf = Number(Mid(Part, StrLen("perf=") + 1));
			Except
				Cfg.perf = 0;
			EndTry;
		ElsIf StrStartsWith(Part, "embedperf=") Then
			// embedperf=<dir> → read every *.feature under <dir>, chunk by scenario,
			// index with REAL embedding and time the whole embedding build.
			Cfg.embedperf = Mid(Part, StrLen("embedperf=") + 1);
		ElsIf StrStartsWith(Part, "batch=") Then
			Try
				Cfg.batch = Number(Mid(Part, StrLen("batch=") + 1));
			Except
				Cfg.batch = 500;
			EndTry;
		ElsIf StrStartsWith(Part, "workers=") Then
			// workers=N → number of concurrent bulk-embedding sessions in the core
			// (0 = auto ≈ ncpu/2; 1 = single worker; N = exactly N).
			Try
				Cfg.workers = Number(Mid(Part, StrLen("workers=") + 1));
			Except
				Cfg.workers = 1;
			EndTry;
		ElsIf Lower(Part) = "synctest" Then
			// Dev-only headless trigger for the Sync-with-vector button: prefills the
			// form fields from steps=/corpus=/coll= and fires SyncVector on open.
			Cfg.synctest = True;
		ElsIf StrStartsWith(Part, "steps=") Then
			Cfg.steps = Mid(Part, StrLen("steps=") + 1);
		ElsIf StrStartsWith(Part, "corpus=") Then
			Cfg.corpus = Mid(Part, StrLen("corpus=") + 1);
		ElsIf StrStartsWith(Part, "coll=") Then
			Cfg.coll = Mid(Part, StrLen("coll=") + 1);
		EndIf;
	EndDo;
	Return Cfg;
EndFunction

&AtClient
Function SelfTestOutDir()
	Dir = "";
	Try
		If TypeOf(SelfTestCfg) = Type("Structure") And ValueIsFilled(SelfTestCfg.out) Then
			Dir = SelfTestCfg.out;
		EndIf;
	Except
	EndTry;
	If Not ValueIsFilled(Dir) Then
		Dir = TempFilesDir();
	EndIf;
	If Right(Dir, 1) <> "\" And Right(Dir, 1) <> "/" Then
		Dir = Dir + "\";
	EndIf;
	Return Dir;
EndFunction

&AtClient
Function SelfTestOutFile(Name)
	Return SelfTestOutDir() + Name;
EndFunction

&AtClient
Function SelfTestAttachCandidates()
	// In-place attach locations for the headless self-test (from launch config):
	// loading the on-disk DLL in ExtCompT puts rcore.dll right beside it.
	L = New Array;
	If TypeOf(SelfTestCfg) = Type("Structure") And ValueIsFilled(SelfTestCfg.extcompt) Then
		L.Add(SelfTestCfg.extcompt);
		Sep = StrFind(SelfTestCfg.extcompt, "\", SearchDirection.FromEnd);
		If Sep > 1 Then
			L.Add(Left(SelfTestCfg.extcompt, Sep - 1));
		EndIf;
	EndIf;
	Return L;
EndFunction

&AtClient
Procedure AttachSelfTestComponent(Index)
	Cands = SelfTestAttachCandidates();
	If Index >= Cands.Count() Then
		TraceLine("all self-test attach candidates failed (no extcompt in launch config?)");
		Return;
	EndIf;
	AddInPath = Cands[Index];
	TraceLine("self-test attach attempt " + String(Index) + " -> " + AddInPath);
	BeginAttachingAddIn(New NotifyDescription("OnOpenAttachEnd", ThisObject, Index),
		AddInPath, "http1c", AddInType.Native);
EndProcedure

&AtClient
Function BuildVersion()
	// Bump on EVERY source change so the log proves a fresh .epf is running.
	Return "selftest-build-23-sync-ui";
EndFunction

&AtClient
Function Ms()
	Return CurrentUniversalDateInMilliseconds();
EndFunction

&AtClient
Procedure TraceLine(Text)
	Try
		W = New TextWriter(SelfTestOutFile("onopen-trace.txt"), TextEncoding.UTF8, , True);
		W.WriteLine("" + Text);
		W.Close();
	Except
	EndTry;
EndProcedure

&AtClient
Procedure OnOpenAttachEnd(Connected, AdditionalParameters) Export

	TraceLine("OnOpenAttachEnd Connected=" + String(Connected) + " source=" + String(AddInPath));
	If Not Connected Then
		// Self-test attaches pass the candidate index; try the next location.
		If TypeOf(AdditionalParameters) = Type("Number") Then
			AttachSelfTestComponent(AdditionalParameters + 1);
		EndIf;
		Return;
	EndIf;
	Try
		Component = New("AddIn.http1c.HttpServer");
		Component.LoggingEnabled = EnableLogging;
		Component.LogPath = LogPath;
		Component.Timeout = 120;
		TraceLine("Component created, version=" + String(Component.Version));
	Except
		TraceLine("Component create EXCEPTION: " + ErrorDescription());
		Return;
	EndTry;

	// If launched for the headless self-test, run the RAG chain now that the
	// component is attached.
	LP = "";
	Try
		LP = Lower(String(LaunchParameter));
	Except
	EndTry;
	If SelfTestCfg.synctest Then
		TraceLine("scheduling Sync_TestTrigger");
		AttachIdleHandler("Sync_TestTrigger", 1, True);
	ElsIf ValueIsFilled(SelfTestCfg.embedperf) Then
		TraceLine("scheduling RunEmbedPerfDeferred");
		AttachIdleHandler("RunEmbedPerfDeferred", 1, True);
	ElsIf StrFind(LP, "ragselftest") > 0 Then
		TraceLine("scheduling RunRagSelfTestDeferred");
		AttachIdleHandler("RunRagSelfTestDeferred", 1, True);
	EndIf;

EndProcedure

#EndRegion


#Region FormCommandHandlers

&AtClient
Procedure Connect(Command)
	
	EnsureLoggingDefaults();

	// Attach-only (no InstallAddIn — it pops a modal install dialog). The
	// component ships in the declared template bundle, so attach loads it directly.
	AddInPath = GetDefaultAddInSource();

	BeginAttachingAddIn(
		New NotifyDescription("AttachAddInEnd", ThisObject),
		AddInPath, "http1c", AddInType.Native);

EndProcedure

&AtClient
Procedure Disconnect(Command)
	
	If Component <> Undefined Then
		Component.BeginCallingStopListen(
			New NotifyDescription("StopListenEnd", ThisObject));
	EndIf;
	
EndProcedure

&AtClient
Procedure StopListenEnd(ResultCall, ParametersCall, AdditionalParameters) Export
	
	Component = Undefined;
	ResetRuntimeStatus();
	ShowMessageBox(, "Server stopped.");
	
EndProcedure

&AtClient
Procedure GetStatus(Command)
	
	If Component = Undefined Then
		ShowMessageBox(, "Component is not connected.");
		Return;
	EndIf;
	
	BuildCombinedStatusJSON(
		New NotifyDescription("GetStatusShowEnd", ThisObject));
	
EndProcedure

&AtClient
Procedure GetStatusShowEnd(StatusJson, AdditionalParameters) Export
	
	ShowMessageBox(, StatusJson);
	
EndProcedure

&AtClient
Procedure TakeScreenshot(Command)
	
	If Component = Undefined Then
		ShowMessageBox(, "Component is not connected.");
		Return;
	EndIf;
	
	PID = ScreenshotPID;
	Format = ScreenshotFormat;
	If Not ValueIsFilled(Format) Then
		Format = "jpeg";
	EndIf;
	Quality = ScreenshotQuality;
	If Quality = 0 Then
		Quality = 80;
	EndIf;
	
	Try
		Component.BeginCallingTakeScreenshot(
			New NotifyDescription("TakeScreenshotFormEnd", ThisObject),
				PID, Format, Quality, ScreenshotGrayscale);
	Except
		ShowMessageBox(, "Screenshot error: " + ErrorDescription());
	EndTry;
EndProcedure

&AtClient
Procedure TakeScreenshotFormEnd(ResultJson, ParametersCall, AdditionalParameters) Export
	
	Try
		JSONReader = New JSONReader;
		JSONReader.SetString(ResultJson);
		CaptureResult = ReadJSON(JSONReader, True);
	Except
		ShowMessageBox(, "Failed to parse result: " + ErrorDescription());
		Return;
	EndTry;
	
	Windows = Undefined;
	If TypeOf(CaptureResult) = Type("Map") Then
		Windows = CaptureResult["windows"];
	ElsIf TypeOf(CaptureResult) = Type("Structure") Then
		CaptureResult.Property("windows", Windows);
	EndIf;
	
	ScreenshotDataArray = New Array;
	ScreenshotCurrentIndex = 0;
	
	If Windows = Undefined Or Windows.Count() = 0 Then
		CaptureError = "";
		If TypeOf(CaptureResult) = Type("Map") Then
			CaptureError = CaptureResult["error"];
		ElsIf TypeOf(CaptureResult) = Type("Structure") Then
			CaptureResult.Property("error", CaptureError);
		EndIf;
		If Not ValueIsFilled(CaptureError) Then
			CaptureError = "No windows captured";
		EndIf;
		ScreenshotInfo = CaptureError;
		ScreenshotPicture = "";
		Return;
	EndIf;
	
	For Each Win In Windows Do
		Title = "";
		ImageData = "";
		IsModal = False;
		IsMainWin = False;
		WinWidth = 0;
		WinHeight = 0;
		WinOwnerIndex = -1;
		WinLevel = 0;
		WinIsEnabled = True;
		WinIsMinimized = False;
		WinIsMaximized = False;
		WinZOrder = 0;
		
		If TypeOf(Win) = Type("Map") Then
			Title = Win["title"];
			ImageData = Win["image"];
			IsModal = Win["isModal"];
			IsMainWin = Win["isMainWindow"];
			WinWidth = Win["width"];
			WinHeight = Win["height"];
			WinOwnerIndex = Win["ownerIndex"];
			WinLevel = Win["level"];
			WinIsEnabled = Win["isEnabled"];
			WinIsMinimized = Win["isMinimized"];
			WinIsMaximized = Win["isMaximized"];
			WinZOrder = Win["zOrder"];
		ElsIf TypeOf(Win) = Type("Structure") Then
			Win.Property("title", Title);
			Win.Property("image", ImageData);
			Win.Property("isModal", IsModal);
			Win.Property("isMainWindow", IsMainWin);
			Win.Property("width", WinWidth);
			Win.Property("height", WinHeight);
			Win.Property("ownerIndex", WinOwnerIndex);
			Win.Property("level", WinLevel);
			Win.Property("isEnabled", WinIsEnabled);
			Win.Property("isMinimized", WinIsMinimized);
			Win.Property("isMaximized", WinIsMaximized);
			Win.Property("zOrder", WinZOrder);
		EndIf;
		
		If Not ValueIsFilled(Title) Then
			Title = "(no title)";
		EndIf;
		
		// Build indented info string showing window hierarchy
		Indent = "";
		LvlIdx = 0;
		While LvlIdx < WinLevel Do
			Indent = Indent + "  ";
			LvlIdx = LvlIdx + 1;
		EndDo;
		
		Info = Indent + Title + " (" + String(WinWidth) + "x" + String(WinHeight) + ")";
		If IsMainWin = True Then
			Info = Info + " [MAIN]";
		EndIf;
		If IsModal = True Then
			Info = Info + " [MODAL]";
		EndIf;
		If WinIsMinimized = True Then
			Info = Info + " [MIN]";
		EndIf;
		If WinIsMaximized = True Then
			Info = Info + " [MAX]";
		EndIf;
		If WinIsEnabled <> True Then
			Info = Info + " [DISABLED]";
		EndIf;
		Info = Info + " [z=" + String(WinZOrder) + "]";
		
		PicAddress = "";
		If ValueIsFilled(ImageData) Then
			BinData = Base64Value(ImageData);
			PicAddress = PutToTempStorage(BinData, ThisObject.UUID);
		EndIf;
		
		Item = New Structure("Title,PictureAddress,Level,OwnerIndex",
			Info, PicAddress, WinLevel, WinOwnerIndex);
		ScreenshotDataArray.Add(Item);
	EndDo;
	
	If ScreenshotDataArray.Count() > 0 Then
		ScreenshotCurrentIndex = 0;
		ShowScreenshotByIndex(ScreenshotCurrentIndex);
	EndIf;
	
EndProcedure

&AtClient
Procedure ShowScreenshotByIndex(Index)
	
	If ScreenshotDataArray = Undefined Or ScreenshotDataArray.Count() = 0 Then
		ScreenshotInfo = "No screenshots";
		ScreenshotPicture = "";
		Return;
	EndIf;
	
	Item = ScreenshotDataArray[Index];
	ScreenshotInfo = "[" + String(Index + 1) + "/" + String(ScreenshotDataArray.Count()) + "] " + Item.Title;
	ScreenshotPicture = Item.PictureAddress;
	
EndProcedure

&AtClient
Procedure PrevScreenshot(Command)
	
	If ScreenshotDataArray = Undefined Or ScreenshotDataArray.Count() = 0 Then
		Return;
	EndIf;
	
	If ScreenshotCurrentIndex > 0 Then
		ScreenshotCurrentIndex = ScreenshotCurrentIndex - 1;
	Else
		ScreenshotCurrentIndex = ScreenshotDataArray.Count() - 1;
	EndIf;
	
	ShowScreenshotByIndex(ScreenshotCurrentIndex);
	
EndProcedure

&AtClient
Procedure NextScreenshot(Command)
	
	If ScreenshotDataArray = Undefined Or ScreenshotDataArray.Count() = 0 Then
		Return;
	EndIf;
	
	If ScreenshotCurrentIndex < ScreenshotDataArray.Count() - 1 Then
		ScreenshotCurrentIndex = ScreenshotCurrentIndex + 1;
	Else
		ScreenshotCurrentIndex = 0;
	EndIf;
	
	ShowScreenshotByIndex(ScreenshotCurrentIndex);
	
EndProcedure

#EndRegion


#Region ComponentLifecycle

&AtClient
Procedure InstallAddInEnd(AdditionalParameters) Export
	
	BeginAttachingAddIn(
		New NotifyDescription("AttachAddInEnd", ThisObject),
		AddInPath,
		"http1c",
		AddInType.Native);
	
EndProcedure

&AtClient
Procedure AttachAddInEnd(Connected, AdditionalParameters) Export
	
	If Not Connected Then
		ShowMessageBox(, "AttachAddIn failed. Source: " + AddInPath);
		Return;
	EndIf;
	
	Try
		Component = New("AddIn.http1c.HttpServer");
	Except
		ShowMessageBox(, "Component creation failed: " + ErrorDescription());
		Return;
	EndTry;

	ResetRuntimeStatus();
	EnsureLoggingDefaults();
	
	// All configuration via synchronous property assignments.
	Try
		Component.LoggingEnabled = EnableLogging;
		Component.LogPath = LogPath;
		Component.Timeout = 120;
	Except
		ShowMessageBox(, "Configuration failed: " + ErrorDescription());
		Return;
	EndTry;
	
	// Register MCP primitives (synchronous property assignments).
	RegisterMCPTools();
	RegisterMCPResources();
	RegisterMCPPrompts();
	
	// Start listening (async — returns via callback).
	PortValue = Port;
	If PortValue = 0 Then
		PortValue = 8888;
	EndIf;
	
	Try
		Component.BeginCallingStartListen(
			New NotifyDescription("StartListenEnd", ThisObject),
			PortValue);
	Except
		ShowMessageBox(, "StartListen failed: " + ErrorDescription());
	EndTry;
	
EndProcedure

&AtClient
Procedure StartListenEnd(ResultCall, ParametersCall, AdditionalParameters) Export
	
	PortValue = Port;
	If PortValue = 0 Then
		PortValue = 8888;
	EndIf;
	ShowMessageBox(, "MCP server started on port " + Format(PortValue, "NG=0"));
	
EndProcedure

&AtClient
Procedure AddInDetachmentOnError(Location, Name)
	
	Component = Undefined;
	MarkRuntimeFailure("Component detached: " + Name + " (" + Location + ")");
	ShowMessageBox(, RuntimeStatus.LastError);
	
EndProcedure

&AtClient
Procedure AttachAddInSSL(TemplateName, SymbolicName, NotifyDescription)
	
	BeginAttachingAddIn(NotifyDescription, TemplateName, SymbolicName, AddInType.Native);
	
EndProcedure

#EndRegion


// ============================================================================
// TOOL DEFINITIONS
// ============================================================================
//
// Each tool is defined as a JSON structure:
//   {
//     "name": "toolName",
//     "description": "What this tool does",
//     "inputSchema": { ... JSON Schema ... },
//     "annotations": { ... optional MCP annotations ... }
//   }
//
// Annotations (optional, per MCP spec):
//   "readOnlyHint"    : true/false - tool does not modify state
//   "destructiveHint" : true/false - tool may irreversibly modify state
//   "idempotentHint"  : true/false - calling multiple times has same effect
//   "openWorldHint"   : true/false - tool interacts with external entities
//
// To add a new tool:
//   1. Create a ToolXxx() function that returns the tool definition
//   2. Add it to the Tools array in RegisterMCPTools()
//   3. Add a handler in ProcessToolCall() dispatcher
//   4. Implement HandleXxx() procedure with Begin* callbacks
// ============================================================================

#Region ToolDefinitions

&AtClient
Procedure RegisterMCPTools()
	
	Tools = New Array;
	Tools.Add(ToolGetStatus());
	Tools.Add(ToolOpenForm());
	Tools.Add(ToolExecute());
	Tools.Add(ToolEvaluate());
	Tools.Add(ToolRunLongTask());
	Tools.Add(ToolTakeScreenshot());
	Tools.Add(ToolTestScreenshot());
	
	Component.Tools = SerializeToJson(Tools);
	
EndProcedure

&AtClient
Function NewTool(Name, Description)
	
	Tool = New Structure;
	Tool.Insert("name", Name);
	Tool.Insert("description", Description);
	Tool.Insert("inputSchema", NewObjectSchema());
	Return Tool;
	
EndFunction

&AtClient
Function NewObjectSchema()
	
	Schema = New Structure;
	Schema.Insert("type", "object");
	Schema.Insert("properties", New Structure);
	Schema.Insert("required", New Array);
	Return Schema;
	
EndFunction

&AtClient
Procedure AddToolParam(Tool, ParamName, ParamType, Description, IsRequired = True)
	
	PropertyDescription = New Structure("type,description", ParamType, Description);
	Tool.inputSchema.properties.Insert(ParamName, PropertyDescription);
	
	If IsRequired Then
		Tool.inputSchema.required.Add(ParamName);
	EndIf;
	
EndProcedure

// Add MCP annotations to a tool definition.
// Annotations help MCP clients understand tool behavior.
//
// Parameters:
//   Tool          - tool structure from NewTool()
//   ReadOnly      - true if the tool does not modify any state
//   Destructive   - true if the tool may irreversibly change data
//   Idempotent    - true if repeated calls produce the same result
//   OpenWorld     - true if the tool contacts external systems
&AtClient
Procedure AddToolAnnotations(Tool, ReadOnly = False, Destructive = False, Idempotent = False, OpenWorld = False)
	
	Annotations = New Structure;
	Annotations.Insert("readOnlyHint", ReadOnly);
	Annotations.Insert("destructiveHint", Destructive);
	Annotations.Insert("idempotentHint", Idempotent);
	Annotations.Insert("openWorldHint", OpenWorld);
	Tool.Insert("annotations", Annotations);
	
EndProcedure

// Build a JSON Schema object for outputSchema.
// Use AddOutputProperty() to add properties, then attach to tool with SetToolOutputSchema().
//
// Example:
//   Schema = NewOutputSchema();
//   AddOutputProperty(Schema, "temperature", "number", "Temperature in Celsius", True);
//   AddOutputProperty(Schema, "status", "string", "Server status text", True);
//   SetToolOutputSchema(Tool, Schema);
&AtClient
Function NewOutputSchema()
	
	Schema = New Structure;
	Schema.Insert("type", "object");
	Schema.Insert("properties", New Structure);
	Schema.Insert("required", New Array);
	Return Schema;
	
EndFunction

// Add a property to an outputSchema.
&AtClient
Procedure AddOutputProperty(Schema, PropName, PropType, Description, IsRequired = True)
	
	PropDef = New Structure("type,description", PropType, Description);
	Schema.properties.Insert(PropName, PropDef);
	
	If IsRequired Then
		Schema.required.Add(PropName);
	EndIf;
	
EndProcedure

// Attach an outputSchema to a tool definition.
// This tells MCP clients the expected structure of the tool's response.
&AtClient
Procedure SetToolOutputSchema(Tool, Schema)
	
	Tool.Insert("outputSchema", Schema);
	
EndProcedure

&AtClient
Function ToolGetStatus()
	
	Tool = NewTool("getStatus",
		"Return the current native component status, logging configuration, and the current 1C runtime state.");
	AddToolAnnotations(Tool, True);  // read-only, safe
	
	// outputSchema — structured result clients can validate
	Schema = NewOutputSchema();
	
	RuntimeProps = New Structure;
	RuntimeProps.Insert("type", "object");
	RuntimeDescription = New Structure;
	RuntimeDescription.Insert("type", "boolean");
	RuntimeDescription.Insert("description", "Whether the runtime is busy processing a request");
	RuntimeProps.Insert("properties", New Structure("IsBusy", RuntimeDescription));
	
	ComponentProps = New Structure;
	ComponentProps.Insert("type", "object");
	ComponentDescription = New Structure;
	ComponentDescription.Insert("type", "boolean");
	ComponentDescription.Insert("description", "Whether the HTTP server is running");
	ComponentProps.Insert("properties", New Structure("running", ComponentDescription));
	
	AddOutputProperty(Schema, "runtimeStatus", "object", "1C runtime state", True);
	AddOutputProperty(Schema, "componentStatus", "object", "Native component state", True);
	SetToolOutputSchema(Tool, Schema);
	
	Return Tool;
	
EndFunction

&AtClient
Function ToolOpenForm()
	
	Tool = NewTool("openForm",
		"Open a 1C:Enterprise form by its full name and return a confirmation message or an error.");
	AddToolParam(Tool, "formPath", "string",
		"Full form path, for example Catalog.Products.ListForm or ExternalDataProcessor.http1c.Form.Form.");
	AddToolParam(Tool, "parameters", "string",
		"Optional JSON string with OpenForm parameters.", False);
	AddToolAnnotations(Tool, False, False, True);  // not read-only, not destructive, idempotent
	
	// outputSchema — confirmation message
	Schema = NewOutputSchema();
	AddOutputProperty(Schema, "message", "string", "Confirmation message or error description");
	SetToolOutputSchema(Tool, Schema);
	
	Return Tool;
	
EndFunction

&AtClient
Function ToolExecute()
	
	Tool = NewTool("execute",
		"Execute arbitrary 1C:Enterprise code on the server. The code must assign a string value to the Result variable.");
	AddToolParam(Tool, "code", "string",
		"Server-side 1C code. Example: Result = String(CurrentDate());");
	AddToolAnnotations(Tool, False, True);  // potentially destructive
	
	// outputSchema — the execution result
	Schema = NewOutputSchema();
	AddOutputProperty(Schema, "result", "string", "Value of the Result variable after execution");
	SetToolOutputSchema(Tool, Schema);
	
	Return Tool;
	
EndFunction

&AtClient
Function ToolEvaluate()
	
	Tool = NewTool("evaluate",
		"Evaluate a 1C:Enterprise expression on the server and return its string representation.");
	AddToolParam(Tool, "expression", "string",
		"Expression text, for example CurrentDate() or Metadata.Documents.Count().");
	AddToolAnnotations(Tool, True, False, True);  // read-only, idempotent
	
	// outputSchema — the evaluation result
	Schema = NewOutputSchema();
	AddOutputProperty(Schema, "result", "string", "String representation of the evaluated expression");
	SetToolOutputSchema(Tool, Schema);
	
	Return Tool;
	
EndFunction

&AtClient
Function ToolRunLongTask()
	
	Tool = NewTool("runLongTask",
		"Run a long test operation and report progress using MCP notifications/progress.");
	AddToolParam(Tool, "steps", "number",
		"Number of progress steps to emit.", False);
	AddToolParam(Tool, "iterationsPerStep", "number",
		"CPU work units performed on the server for each step.", False);
	AddToolAnnotations(Tool, True, False, False);  // read-only (test/benchmark)
	
	// outputSchema — task completion summary
	Schema = NewOutputSchema();
	AddOutputProperty(Schema, "completedSteps", "number", "Total number of steps completed");
	AddOutputProperty(Schema, "summary", "string", "Summary of the completed task");
	SetToolOutputSchema(Tool, Schema);
	
	Return Tool;
	
EndFunction

&AtClient
Function ToolTakeScreenshot()
	
	Tool = NewTool("takeScreenshot",
		"Capture screenshots of all visible windows of the 1C:Enterprise process, including modal dialogs. Returns base64-encoded JPEG images by default (smaller size, optimal for AI). Supports PNG for lossless quality. Use grayscale=true to reduce image size when color is not needed.");
	AddToolParam(Tool, "pid", "number",
		"Process ID of the target 1C:Enterprise instance. Use 0 or omit to capture the current process.", False);
	AddToolParam(Tool, "format", "string",
		"Image format: 'jpeg' (default, smaller size) or 'png' (lossless).", False);
	AddToolParam(Tool, "quality", "number",
		"JPEG compression quality 1-100 (default 80). Lower values = smaller files. Ignored for PNG.", False);
	AddToolParam(Tool, "grayscale", "boolean",
		"Convert to grayscale (default false). Reduces file size; useful when color is not needed by the AI.", False);
	AddToolAnnotations(Tool, True);  // read-only, safe
	
	Return Tool;
	
EndFunction

&AtClient
Function ToolTestScreenshot()
	
	Tool = NewTool("testScreenshot",
		"Test tool for capturing screenshots and returning them as MCP image content. Takes 3 required parameters.");
	AddToolParam(Tool, "pid", "number",
		"Process ID of the target process. Use 0 for the current 1C process.");
	AddToolParam(Tool, "format", "string",
		"Image format: 'jpeg' or 'png'.");
	AddToolParam(Tool, "quality", "number",
		"Compression quality 1-100 (for JPEG). Ignored for PNG.");
	AddToolParam(Tool, "grayscale", "boolean",
		"Convert to grayscale. Reduces file size when color is not needed.");
	AddToolAnnotations(Tool, True);  // read-only, safe
	
	Return Tool;
	
EndFunction

#EndRegion


#Region ToolDispatcher

&AtClient
Procedure ProcessToolCall(Data)
	
	Try
		JSONReader = New JSONReader;
		JSONReader.SetString(Data);
		Request = ReadJSON(JSONReader, True);
	Except
		Return;
	EndTry;
	
	RequestID = Request["id"];
	ToolName = Request["tool"];
	Arguments = Request["arguments"];
	
	If Arguments = Undefined Then
		Arguments = New Structure;
	EndIf;
	
	Try
		If ToolName = "getStatus" Then
			HandleGetStatus(RequestID);
		ElsIf ToolName = "openForm" Then
			HandleOpenForm(RequestID, Arguments);
		ElsIf ToolName = "execute" Then
			HandleExecute(RequestID, Arguments);
		ElsIf ToolName = "evaluate" Then
			HandleEvaluate(RequestID, Arguments);
		ElsIf ToolName = "runLongTask" Then
			HandleRunLongTask(RequestID, Arguments);
		ElsIf ToolName = "takeScreenshot" Then
			HandleTakeScreenshot(RequestID, Arguments);
		ElsIf ToolName = "testScreenshot" Then
			HandleTestScreenshot(RequestID, Arguments);
		Else
			SendToolError(RequestID, "Unknown tool: " + ToolName);
		EndIf;
	Except
		MarkRuntimeFailure("Dispatcher error for tool '" + ToolName + "': " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
	EndTry;
	
EndProcedure

#EndRegion


// ============================================================================
// RESOURCE DEFINITIONS
// ============================================================================
//
// MCP Resources provide contextual data to AI clients. Unlike tools, resources
// are read-only data that can be listed and fetched.
//
// Each resource is defined as:
//   {
//     "uri": "1c://metadata/catalogs",
//     "name": "1C Catalogs List",
//     "description": "List of all catalog metadata objects",
//     "mimeType": "application/json"
//   }
//
// When a client calls resources/read with a URI, the native component sends
// a "ResourceRead" ExternalEvent to 1C. The handler should respond with
// the resource content via SendResponse().
//
// To add a new resource:
//   1. Add a resource definition to RegisterMCPResources()
//   2. Add a handler in ProcessResourceRead() dispatcher
// ============================================================================

#Region ResourceDefinitions

&AtClient
Procedure RegisterMCPResources()
	
	Resources = New Array;
	
	// Example: expose 1C metadata catalog list as a resource
	Resource = New Structure;
	Resource.Insert("uri", "1c://metadata/catalogs");
	Resource.Insert("name", "1C Catalogs Metadata");
	Resource.Insert("description",
		"JSON list of all catalog (directory) metadata objects in the current 1C infobase, including their names and synonyms.");
	Resource.Insert("mimeType", "application/json");
	Resources.Add(Resource);
	
	// Example: expose 1C metadata document list as a resource
	Resource = New Structure;
	Resource.Insert("uri", "1c://metadata/documents");
	Resource.Insert("name", "1C Documents Metadata");
	Resource.Insert("description",
		"JSON list of all document metadata objects in the current 1C infobase.");
	Resource.Insert("mimeType", "application/json");
	Resources.Add(Resource);
	
	Component.Resources = SerializeToJson(Resources);
	
EndProcedure

#EndRegion


// ============================================================================
// PROMPT DEFINITIONS
// ============================================================================
//
// MCP Prompts are reusable interaction templates. They help AI applications
// structure their interactions with the 1C system.
//
// Each prompt is defined as:
//   {
//     "name": "promptName",
//     "description": "What this prompt template does",
//     "arguments": [
//       { "name": "argName", "description": "...", "required": true }
//     ]
//   }
//
// When a client calls prompts/get, the native component sends a "PromptGet"
// ExternalEvent. The handler returns a messages array per MCP spec.
//
// To add a new prompt:
//   1. Add a prompt definition to RegisterMCPPrompts()
//   2. Add a handler in ProcessPromptGet() dispatcher
// ============================================================================

#Region PromptDefinitions

&AtClient
Procedure RegisterMCPPrompts()
	
	Prompts = New Array;
	
	// Example: a prompt template for analyzing 1C data
	Prompt = New Structure;
	Prompt.Insert("name", "analyze1CData");
	Prompt.Insert("description",
		"Generate a system prompt for analyzing data in a 1C:Enterprise infobase. "
		+ "Provides context about available metadata and suggests analysis approaches.");
	PromptArgs = New Array;
	Arg = New Structure("name,description,required", "topic",
		"Analysis topic or area of interest (e.g., sales, inventory, HR).", False);
	PromptArgs.Add(Arg);
	Prompt.Insert("arguments", PromptArgs);
	Prompts.Add(Prompt);
	
	// Example: a prompt for generating 1C code
	Prompt = New Structure;
	Prompt.Insert("name", "generate1CCode");
	Prompt.Insert("description",
		"Generate a system prompt optimized for writing 1C:Enterprise BSL code. "
		+ "Includes coding conventions, common patterns, and available API references.");
	PromptArgs = New Array;
	Arg = New Structure("name,description,required", "task",
		"Description of the coding task.", True);
	PromptArgs.Add(Arg);
	Prompt.Insert("arguments", PromptArgs);
	Prompts.Add(Prompt);
	
	Component.Prompts = SerializeToJson(Prompts);
	
EndProcedure

#EndRegion


// ============================================================================
// RESOURCE DISPATCHER
// ============================================================================

#Region ResourceDispatcher

&AtClient
Procedure ProcessResourceRead(Data)
	
	Try
		JSONReader = New JSONReader;
		JSONReader.SetString(Data);
		Request = ReadJSON(JSONReader, True);
	Except
		Return;
	EndTry;
	
	RequestID = Request["id"];
	URI = Request["uri"];
	
	Try
		If URI = "1c://metadata/catalogs" Then
			HandleReadCatalogs(RequestID, URI);
		ElsIf URI = "1c://metadata/documents" Then
			HandleReadDocuments(RequestID, URI);
		Else
			SendResourceError(RequestID, "Unknown resource URI: " + URI);
		EndIf;
	Except
		SendResourceError(RequestID, "Resource read error: " + ErrorDescription());
	EndTry;
	
EndProcedure

#EndRegion


// ============================================================================
// PROMPT DISPATCHER
// ============================================================================

#Region PromptDispatcher

&AtClient
Procedure ProcessPromptGet(Data)
	
	Try
		JSONReader = New JSONReader;
		JSONReader.SetString(Data);
		Request = ReadJSON(JSONReader, True);
	Except
		Return;
	EndTry;
	
	RequestID = Request["id"];
	PromptName = Request["name"];
	Arguments = Request["arguments"];
	
	If Arguments = Undefined Then
		Arguments = New Structure;
	EndIf;
	
	Try
		If PromptName = "analyze1CData" Then
			HandlePromptAnalyze(RequestID, Arguments);
		ElsIf PromptName = "generate1CCode" Then
			HandlePromptGenerateCode(RequestID, Arguments);
		Else
			SendPromptError(RequestID, "Unknown prompt: " + PromptName);
		EndIf;
	Except
		SendPromptError(RequestID, "Prompt error: " + ErrorDescription());
	EndTry;
	
EndProcedure

#EndRegion


#Region ToolHandlers

&AtClient
Procedure HandleGetStatus(RequestID)
	
	Try
		Context = New Structure("RequestID", RequestID);
		BuildCombinedStatusJSON(
			New NotifyDescription("HandleGetStatusEnd", ThisObject, Context));
	Except
		MarkRuntimeFailure("Status retrieval failed: " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
	EndTry;
	
EndProcedure

&AtClient
Procedure HandleGetStatusEnd(StatusJson, AdditionalParameters) Export
	
	SendToolResult(AdditionalParameters.RequestID, StatusJson);
	
EndProcedure

&AtClient
Procedure HandleOpenForm(RequestID, Arguments)
	
	FormPath = GetArg(Arguments, "formPath");
	If Not ValueIsFilled(FormPath) Then
		SendToolError(RequestID, "Parameter 'formPath' is required.");
		Return;
	EndIf;
	
	MarkRuntimeStart(RequestID, "openForm", 1);
	SendToolProgress(RequestID, 0, 1, "Preparing to open the form.");
	
	Try
		ParametersJson = GetArg(Arguments, "parameters");
		FormParameters = ParseJsonArgument(ParametersJson, New Structure);
		OpenForm(FormPath, FormParameters);
		MarkRuntimeProgress(1, 1, "The form was opened successfully.");
		SendToolProgress(RequestID, 1, 1, RuntimeStatus.ProgressMessage);
		MarkRuntimeSuccess("Form opened: " + FormPath);
		SendToolResult(RequestID, RuntimeStatus.LastResult);
	Except
		MarkRuntimeFailure("Failed to open form '" + FormPath + "': " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
	EndTry;
	
EndProcedure

&AtClient
Procedure HandleExecute(RequestID, Arguments)
	
	Code = GetArg(Arguments, "code");
	If Not ValueIsFilled(Code) Then
		SendToolError(RequestID, "Parameter 'code' is required.");
		Return;
	EndIf;
	
	MarkRuntimeStart(RequestID, "execute", 1);
	SendToolProgress(RequestID, 0, 1, "Executing server code.");
	
	Try
		ExecutionResult = ExecuteCodeOnServer(Code);
		MarkRuntimeProgress(1, 1, "Server code execution completed.");
		SendToolProgress(RequestID, 1, 1, RuntimeStatus.ProgressMessage);
		MarkRuntimeSuccess(ExecutionResult);
		SendToolResult(RequestID, RuntimeStatus.LastResult);
	Except
		MarkRuntimeFailure("Execution error: " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
	EndTry;
	
EndProcedure

&AtServer
Function ExecuteCodeOnServer(Val Code)
	
	Result = "";
	Execute(Code);
	Return String(Result);
	
EndFunction

&AtClient
Procedure HandleEvaluate(RequestID, Arguments)
	
	Expression = GetArg(Arguments, "expression");
	If Not ValueIsFilled(Expression) Then
		SendToolError(RequestID, "Parameter 'expression' is required.");
		Return;
	EndIf;
	
	MarkRuntimeStart(RequestID, "evaluate", 1);
	SendToolProgress(RequestID, 0, 1, "Evaluating expression.");
	
	Try
		EvaluationResult = EvaluateOnServer(Expression);
		MarkRuntimeProgress(1, 1, "Expression evaluation completed.");
		SendToolProgress(RequestID, 1, 1, RuntimeStatus.ProgressMessage);
		MarkRuntimeSuccess(EvaluationResult);
		SendToolResult(RequestID, RuntimeStatus.LastResult);
	Except
		MarkRuntimeFailure("Evaluation error: " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
	EndTry;
	
EndProcedure

&AtServer
Function EvaluateOnServer(Val Expression)
	
	Result = Eval(Expression);
	Return String(Result);
	
EndFunction

&AtClient
Procedure HandleRunLongTask(RequestID, Arguments)
	
	Steps = Max(1, NumberOrDefault(GetArg(Arguments, "steps"), 5));
	IterationsPerStep = Max(1, NumberOrDefault(GetArg(Arguments, "iterationsPerStep"), 4000000));
	
	MarkRuntimeStart(RequestID, "runLongTask", Steps);
	SendToolProgress(RequestID, 0, Steps, "Long-running test task started.");
	
	Try
		For StepIndex = 1 To Steps Do
			WorkSummary = RunLongTaskStepOnServer(IterationsPerStep, StepIndex, Steps);
			MarkRuntimeProgress(StepIndex, Steps, WorkSummary);
			SendToolProgress(RequestID, StepIndex, Steps, RuntimeStatus.ProgressMessage);
		EndDo;
		
		MarkRuntimeSuccess("Long-running test task completed.");
		SendToolResult(RequestID, RuntimeStatus.LastResult);
	Except
		MarkRuntimeFailure("Long-running task failed: " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
	EndTry;
	
EndProcedure

&AtServer
Function RunLongTaskStepOnServer(Val IterationsPerStep, Val StepIndex, Val TotalSteps)
	
	Accumulator = 0;
	For Counter = 1 To IterationsPerStep Do
		Accumulator = Accumulator + (Counter % 97);
	EndDo;
	
	Return "Completed step " + String(StepIndex) + " of " + String(TotalSteps)
		+ ". Accumulator=" + String(Accumulator);
	
EndFunction

&AtClient
Procedure HandleTakeScreenshot(RequestID, Arguments)
	
	PID = NumberOrDefault(GetArg(Arguments, "pid"), 0);
	
	Format = "jpeg";
	ArgFormat = GetArg(Arguments, "format");
	If ValueIsFilled(ArgFormat) Then
		Format = ArgFormat;
	EndIf;
	
	Quality = NumberOrDefault(GetArg(Arguments, "quality"), 80);
	
	Grayscale = False;
	ArgGrayscale = GetArg(Arguments, "grayscale");
	If ArgGrayscale = True Then
		Grayscale = True;
	EndIf;
	
	MarkRuntimeStart(RequestID, "takeScreenshot", 1);
	SendToolProgress(RequestID, 0, 1, "Capturing screenshots...");
	
	Try
		Context = New Structure("RequestID", RequestID);
		Component.BeginCallingTakeScreenshot(
			New NotifyDescription("HandleTakeScreenshotEnd", ThisObject, Context),
			PID, Format, Quality, Grayscale);
	Except
		MarkRuntimeFailure("Screenshot capture failed: " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
	EndTry;
	
EndProcedure

&AtClient
Procedure HandleTakeScreenshotEnd(ResultJson, ParametersCall, AdditionalParameters) Export
	
	RequestID = AdditionalParameters.RequestID;
	
	Try
		JSONReader = New JSONReader;
		JSONReader.SetString(ResultJson);
		CaptureResult = ReadJSON(JSONReader, True);
	Except
		MarkRuntimeFailure("Failed to parse screenshot result: " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
		Return;
	EndTry;
	
	CaptureError = Undefined;
	If TypeOf(CaptureResult) = Type("Map") Then
		CaptureError = CaptureResult["error"];
	ElsIf TypeOf(CaptureResult) = Type("Structure") Then
		CaptureResult.Property("error", CaptureError);
	EndIf;
	
	Windows = Undefined;
	If TypeOf(CaptureResult) = Type("Map") Then
		Windows = CaptureResult["windows"];
	ElsIf TypeOf(CaptureResult) = Type("Structure") Then
		CaptureResult.Property("windows", Windows);
	EndIf;
	
	If Windows = Undefined Or Windows.Count() = 0 Then
		If Not ValueIsFilled(CaptureError) Then
			CaptureError = "No windows captured";
		EndIf;
		MarkRuntimeFailure(CaptureError);
		SendToolError(RequestID, CaptureError);
		Return;
	EndIf;
	
	// Build MCP content array with images and descriptions.
	Content = New Array;
	
	PID = 0;
	If TypeOf(CaptureResult) = Type("Map") Then
		PID = CaptureResult["pid"];
	ElsIf TypeOf(CaptureResult) = Type("Structure") Then
		CaptureResult.Property("pid", PID);
	EndIf;
	
	SummaryItem = New Structure("type,text", "text",
		"Captured " + String(Windows.Count()) + " window(s) for PID " + String(PID));
	Content.Add(SummaryItem);
	
	For Each Win In Windows Do
		Title = "";
		If TypeOf(Win) = Type("Map") Then
			Title = Win["title"];
		ElsIf TypeOf(Win) = Type("Structure") Then
			Win.Property("title", Title);
		EndIf;
		If Not ValueIsFilled(Title) Then
			Title = "(no title)";
		EndIf;
		
		WinWidth = 0;
		WinHeight = 0;
		IsModal = False;
		IsMainWin = False;
		ImageData = "";
		MimeType = "image/jpeg";
		WinError = "";
		WinLevel = 0;
		WinOwnerIndex = -1;
		WinZOrder = 0;
		WinIsEnabled = True;
		WinIsMinimized = False;
		WinIsMaximized = False;
		
		If TypeOf(Win) = Type("Map") Then
			WinWidth = Win["width"];
			WinHeight = Win["height"];
			IsModal = Win["isModal"];
			IsMainWin = Win["isMainWindow"];
			ImageData = Win["image"];
			MimeType = Win["mimeType"];
			WinError = Win["error"];
			WinLevel = Win["level"];
			WinOwnerIndex = Win["ownerIndex"];
			WinZOrder = Win["zOrder"];
			WinIsEnabled = Win["isEnabled"];
			WinIsMinimized = Win["isMinimized"];
			WinIsMaximized = Win["isMaximized"];
		ElsIf TypeOf(Win) = Type("Structure") Then
			Win.Property("width", WinWidth);
			Win.Property("height", WinHeight);
			Win.Property("isModal", IsModal);
			Win.Property("isMainWindow", IsMainWin);
			Win.Property("image", ImageData);
			Win.Property("mimeType", MimeType);
			Win.Property("error", WinError);
			Win.Property("level", WinLevel);
			Win.Property("ownerIndex", WinOwnerIndex);
			Win.Property("zOrder", WinZOrder);
			Win.Property("isEnabled", WinIsEnabled);
			Win.Property("isMinimized", WinIsMinimized);
			Win.Property("isMaximized", WinIsMaximized);
		EndIf;
		
		// Build indented info string for AI (shows hierarchy via level)
		AIIndent = "";
		AILvlIdx = 0;
		While AILvlIdx < WinLevel Do
			AIIndent = AIIndent + "  ";
			AILvlIdx = AILvlIdx + 1;
		EndDo;
		Info = AIIndent + Title + " (" + String(WinWidth) + "x" + String(WinHeight) + ")";
		If IsMainWin = True Then
			Info = Info + " [MAIN]";
		EndIf;
		If IsModal = True Then
			Info = Info + " [MODAL]";
		EndIf;
		If WinIsMinimized = True Then
			Info = Info + " [MIN]";
		EndIf;
		If WinIsMaximized = True Then
			Info = Info + " [MAX]";
		EndIf;
		If WinIsEnabled <> True Then
			Info = Info + " [DISABLED]";
		EndIf;
		Info = Info + " [z=" + String(WinZOrder) + "]";
		If WinOwnerIndex >= 0 Then
			Info = Info + " [owner=#" + String(WinOwnerIndex) + "]";
		EndIf;
		
		TextItem = New Structure("type,text", "text", Info);
		Content.Add(TextItem);
		
		If ValueIsFilled(ImageData) Then
			ImageItem = New Structure("type,data,mimeType", "image", ImageData, MimeType);
			Content.Add(ImageItem);
		Else
			If Not ValueIsFilled(WinError) Then
				WinError = "Capture failed";
			EndIf;
			ErrItem = New Structure("type,text", "text", "Error: " + WinError);
			Content.Add(ErrItem);
		EndIf;
	EndDo;
	
	MarkRuntimeProgress(1, 1, "Screenshot captured: " + String(Windows.Count()) + " window(s)");
	SendToolProgress(RequestID, 1, 1, RuntimeStatus.ProgressMessage);
	MarkRuntimeSuccess("Screenshot captured: " + String(Windows.Count()) + " window(s)");
	
	// Send custom content with images (bypassing SendToolResult which only creates text).
	ToolResult = New Structure;
	ToolResult.Insert("content", Content);
	SendMCPResponse(RequestID, ToolResult);
	
EndProcedure

&AtClient
Procedure HandleTestScreenshot(RequestID, Arguments)
	
	PID = NumberOrDefault(GetArg(Arguments, "pid"), 0);
	
	Format = "jpeg";
	ArgFormat = GetArg(Arguments, "format");
	If ValueIsFilled(ArgFormat) Then
		Format = ArgFormat;
	EndIf;
	
	Quality = NumberOrDefault(GetArg(Arguments, "quality"), 80);
	
	Grayscale = False;
	ArgGrayscale = GetArg(Arguments, "grayscale");
	If ArgGrayscale = True Then
		Grayscale = True;
	EndIf;
	
	MarkRuntimeStart(RequestID, "testScreenshot", 1);
	SendToolProgress(RequestID, 0, 1, "Test: capturing screenshots...");
	
	Try
		Context = New Structure("RequestID", RequestID);
		Component.BeginCallingTakeScreenshot(
			New NotifyDescription("HandleTestScreenshotEnd", ThisObject, Context),
			PID, Format, Quality, Grayscale);
	Except
		MarkRuntimeFailure("Test screenshot failed: " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
	EndTry;
	
EndProcedure

&AtClient
Procedure HandleTestScreenshotEnd(ResultJson, ParametersCall, AdditionalParameters) Export
	
	RequestID = AdditionalParameters.RequestID;
	
	Try
		JSONReader = New JSONReader;
		JSONReader.SetString(ResultJson);
		CaptureResult = ReadJSON(JSONReader, True);
	Except
		MarkRuntimeFailure("Failed to parse test screenshot result: " + ErrorDescription());
		SendToolError(RequestID, RuntimeStatus.LastError);
		Return;
	EndTry;
	
	CaptureError = Undefined;
	If TypeOf(CaptureResult) = Type("Map") Then
		CaptureError = CaptureResult["error"];
	ElsIf TypeOf(CaptureResult) = Type("Structure") Then
		CaptureResult.Property("error", CaptureError);
	EndIf;
	
	Windows = Undefined;
	If TypeOf(CaptureResult) = Type("Map") Then
		Windows = CaptureResult["windows"];
	ElsIf TypeOf(CaptureResult) = Type("Structure") Then
		CaptureResult.Property("windows", Windows);
	EndIf;
	
	If Windows = Undefined Or Windows.Count() = 0 Then
		If Not ValueIsFilled(CaptureError) Then
			CaptureError = "No windows captured";
		EndIf;
		MarkRuntimeFailure(CaptureError);
		SendToolError(RequestID, CaptureError);
		Return;
	EndIf;
	
	Content = New Array;
	
	PID = 0;
	If TypeOf(CaptureResult) = Type("Map") Then
		PID = CaptureResult["pid"];
	ElsIf TypeOf(CaptureResult) = Type("Structure") Then
		CaptureResult.Property("pid", PID);
	EndIf;
	
	SummaryItem = New Structure("type,text", "text",
		"[TEST] Captured " + String(Windows.Count()) + " window(s) for PID " + String(PID));
	Content.Add(SummaryItem);
	
	For Each Win In Windows Do
		Title = "";
		MimeType = "image/jpeg";
		ImageData = "";
		IsModal = False;
		IsMainWin = False;
		WinWidth = 0;
		WinHeight = 0;
		WinError = "";
		
		If TypeOf(Win) = Type("Map") Then
			Title = Win["title"];
			MimeType = Win["mimeType"];
			ImageData = Win["image"];
			IsModal = Win["isModal"];
			IsMainWin = Win["isMainWindow"];
			WinWidth = Win["width"];
			WinHeight = Win["height"];
			WinError = Win["error"];
		ElsIf TypeOf(Win) = Type("Structure") Then
			Win.Property("title", Title);
			Win.Property("mimeType", MimeType);
			Win.Property("image", ImageData);
			Win.Property("isModal", IsModal);
			Win.Property("isMainWindow", IsMainWin);
			Win.Property("width", WinWidth);
			Win.Property("height", WinHeight);
			Win.Property("error", WinError);
		EndIf;
		
		If Not ValueIsFilled(Title) Then
			Title = "(no title)";
		EndIf;
		
		Info = Title + " (" + String(WinWidth) + "x" + String(WinHeight) + ")";
		If IsModal = True Then
			Info = Info + " [MODAL]";
		EndIf;
		If IsMainWin = True Then
			Info = Info + " [MAIN]";
		EndIf;
		
		TextItem = New Structure("type,text", "text", Info);
		Content.Add(TextItem);
		
		If ValueIsFilled(ImageData) Then
			ImageItem = New Structure("type,data,mimeType", "image", ImageData, MimeType);
			Content.Add(ImageItem);
		Else
			If Not ValueIsFilled(WinError) Then
				WinError = "Capture failed";
			EndIf;
			ErrItem = New Structure("type,text", "text", "Error: " + WinError);
			Content.Add(ErrItem);
		EndIf;
	EndDo;
	
	MarkRuntimeProgress(1, 1, "[TEST] Screenshot captured: " + String(Windows.Count()) + " window(s)");
	SendToolProgress(RequestID, 1, 1, RuntimeStatus.ProgressMessage);
	MarkRuntimeSuccess("[TEST] Screenshot captured: " + String(Windows.Count()) + " window(s)");
	
	ToolResult = New Structure;
	ToolResult.Insert("content", Content);
	SendMCPResponse(RequestID, ToolResult);
	
EndProcedure

#EndRegion
// ============================================================================
//
// Each handler reads data from the 1C infobase and sends it back in
// MCP resources/read response format:
//   {
//     "contents": [
//       { "uri": "1c://...", "mimeType": "application/json", "text": "..." }
//     ]
//   }
// ============================================================================

#Region ResourceHandlers

&AtClient
Procedure HandleReadCatalogs(RequestID, URI)
	
	CatalogData = GetCatalogMetadataOnServer();
	
	ResourceResult = New Structure;
	Contents = New Array;
	ContentItem = New Structure;
	ContentItem.Insert("uri", URI);
	ContentItem.Insert("mimeType", "application/json");
	ContentItem.Insert("text", CatalogData);
	Contents.Add(ContentItem);
	ResourceResult.Insert("contents", Contents);
	
	SendResourceResult(RequestID, ResourceResult);
	
EndProcedure

&AtServer
Function GetCatalogMetadataOnServer()
	
	CatalogList = New Array;
	For Each Cat In Metadata.Catalogs Do
		Item = New Structure;
		Item.Insert("name", Cat.Name);
		Item.Insert("synonym", String(Cat.Synonym));
		Item.Insert("attributeCount", Cat.Attributes.Count());
		Item.Insert("tabularsCount", Cat.TabularSections.Count());
		CatalogList.Add(Item);
	EndDo;
	
	JSONWriter = New JSONWriter;
	JSONWriter.SetString();
	WriteJSON(JSONWriter, CatalogList);
	Return JSONWriter.Close();
	
EndFunction

&AtClient
Procedure HandleReadDocuments(RequestID, URI)
	
	DocumentData = GetDocumentMetadataOnServer();
	
	ResourceResult = New Structure;
	Contents = New Array;
	ContentItem = New Structure;
	ContentItem.Insert("uri", URI);
	ContentItem.Insert("mimeType", "application/json");
	ContentItem.Insert("text", DocumentData);
	Contents.Add(ContentItem);
	ResourceResult.Insert("contents", Contents);
	
	SendResourceResult(RequestID, ResourceResult);
	
EndProcedure

&AtServer
Function GetDocumentMetadataOnServer()
	
	DocumentList = New Array;
	For Each Doc In Metadata.Documents Do
		Item = New Structure;
		Item.Insert("name", Doc.Name);
		Item.Insert("synonym", String(Doc.Synonym));
		Item.Insert("attributeCount", Doc.Attributes.Count());
		Item.Insert("tabularsCount", Doc.TabularSections.Count());
		DocumentList.Add(Item);
	EndDo;
	
	JSONWriter = New JSONWriter;
	JSONWriter.SetString();
	WriteJSON(JSONWriter, DocumentList);
	Return JSONWriter.Close();
	
EndFunction

#EndRegion


// ============================================================================
// PROMPT HANDLERS
// ============================================================================
//
// Each handler builds an MCP messages array per the prompts/get response format:
//   {
//     "messages": [
//       { "role": "user", "content": { "type": "text", "text": "..." } }
//     ]
//   }
// ============================================================================

#Region PromptHandlers

&AtClient
Procedure HandlePromptAnalyze(RequestID, Arguments)
	
	Topic = GetArg(Arguments, "topic");
	If Not ValueIsFilled(Topic) Then
		Topic = "general overview";
	EndIf;
	
	MetadataSummary = GetMetadataSummaryOnServer();
	
	PromptText = "You are an expert 1C:Enterprise data analyst. "
		+ "Analyze the following data from a 1C infobase on the topic: " + Topic + "."
		+ Chars.LF + Chars.LF
		+ "Available metadata in the infobase:" + Chars.LF + MetadataSummary
		+ Chars.LF + Chars.LF
		+ "Use the 'evaluate' and 'execute' tools to query data and perform analysis. "
		+ "Present findings in a structured format with actionable insights.";
	
	Messages = New Array;
	Message = New Structure;
	Message.Insert("role", "user");
	Content = New Structure("type,text", "text", PromptText);
	Message.Insert("content", Content);
	Messages.Add(Message);
	
	PromptResult = New Structure("messages", Messages);
	SendPromptResult(RequestID, PromptResult);
	
EndProcedure

&AtServer
Function GetMetadataSummaryOnServer()
	
	Lines = New Array;
	Lines.Add("Catalogs: " + String(Metadata.Catalogs.Count()));
	Lines.Add("Documents: " + String(Metadata.Documents.Count()));
	Lines.Add("Information Registers: " + String(Metadata.InformationRegisters.Count()));
	Lines.Add("Accumulation Registers: " + String(Metadata.AccumulationRegisters.Count()));
	
	For Each Cat In Metadata.Catalogs Do
		Lines.Add("  - Catalog." + Cat.Name + " (" + String(Cat.Synonym) + ")");
	EndDo;
	For Each Doc In Metadata.Documents Do
		Lines.Add("  - Document." + Doc.Name + " (" + String(Doc.Synonym) + ")");
	EndDo;

	// One StrConcat over the ready array instead of +-in-a-loop (which reallocates
	// the whole growing string on every iteration). Trailing LF preserved.
	Return StrConcat(Lines, Chars.LF) + Chars.LF;

EndFunction

&AtClient
Procedure HandlePromptGenerateCode(RequestID, Arguments)
	
	Task = GetArg(Arguments, "task");
	If Not ValueIsFilled(Task) Then
		SendPromptError(RequestID, "Parameter 'task' is required.");
		Return;
	EndIf;
	
	PromptText = "You are an expert 1C:Enterprise BSL developer. "
		+ "Write clean, well-documented 1C code for the following task: " + Task + "."
		+ Chars.LF + Chars.LF
		+ "Follow these coding conventions:"
		+ Chars.LF + "- Use meaningful Russian or English variable names"
		+ Chars.LF + "- Add comments explaining business logic"
		+ Chars.LF + "- Use Begin* callback pattern for component calls"
		+ Chars.LF + "- Handle errors with Try/Except"
		+ Chars.LF + "- Separate server and client code with proper directives"
		+ Chars.LF + Chars.LF
		+ "You can test code using the 'execute' tool by assigning the result to the Result variable. "
		+ "Use 'evaluate' for quick expression checks.";
	
	Messages = New Array;
	Message = New Structure;
	Message.Insert("role", "user");
	Content = New Structure("type,text", "text", PromptText);
	Message.Insert("content", Content);
	Messages.Add(Message);
	
	PromptResult = New Structure("messages", Messages);
	SendPromptResult(RequestID, PromptResult);
	
EndProcedure

#EndRegion


#Region RuntimeStatus

&AtClient
Procedure ResetRuntimeStatus()
	
	RuntimeStatus = New Structure;
	RuntimeStatus.Insert("IsBusy", False);
	RuntimeStatus.Insert("CurrentRequestID", "");
	RuntimeStatus.Insert("CurrentTool", "");
	RuntimeStatus.Insert("Progress", 0);
	RuntimeStatus.Insert("Total", 0);
	RuntimeStatus.Insert("ProgressMessage", "Idle");
	RuntimeStatus.Insert("LastResult", "");
	RuntimeStatus.Insert("LastError", "");
	RuntimeStatus.Insert("UpdatedAt", String(CurrentDate()));
	
EndProcedure

&AtClient
Procedure MarkRuntimeStart(RequestID, ToolName, Total)
	
	EnsureRuntimeStatus();
	RuntimeStatus.IsBusy = True;
	RuntimeStatus.CurrentRequestID = RequestID;
	RuntimeStatus.CurrentTool = ToolName;
	RuntimeStatus.Progress = 0;
	RuntimeStatus.Total = Total;
	RuntimeStatus.ProgressMessage = "Started.";
	RuntimeStatus.LastError = "";
	RuntimeStatus.UpdatedAt = String(CurrentDate());
	
EndProcedure

&AtClient
Procedure MarkRuntimeProgress(Progress, Total, Message)
	
	EnsureRuntimeStatus();
	RuntimeStatus.Progress = Progress;
	RuntimeStatus.Total = Total;
	RuntimeStatus.ProgressMessage = Message;
	RuntimeStatus.UpdatedAt = String(CurrentDate());
	
EndProcedure

&AtClient
Procedure MarkRuntimeSuccess(ResultText)
	
	EnsureRuntimeStatus();
	RuntimeStatus.IsBusy = False;
	RuntimeStatus.LastResult = ResultText;
	RuntimeStatus.LastError = "";
	RuntimeStatus.ProgressMessage = "Completed.";
	RuntimeStatus.UpdatedAt = String(CurrentDate());
	
EndProcedure

&AtClient
Procedure MarkRuntimeFailure(ErrorText)
	
	EnsureRuntimeStatus();
	RuntimeStatus.IsBusy = False;
	RuntimeStatus.LastError = ErrorText;
	RuntimeStatus.ProgressMessage = "Failed.";
	RuntimeStatus.UpdatedAt = String(CurrentDate());
	
EndProcedure

&AtClient
Procedure EnsureRuntimeStatus()
	
	If RuntimeStatus = Undefined Then
		ResetRuntimeStatus();
	EndIf;
	
EndProcedure

&AtClient
Procedure EnsureLoggingDefaults()
	
	If DefaultLogPath = Undefined Then
		DefaultLogPath = "http_debug.log";
	EndIf;
	
	If LogPath = Undefined Or Not ValueIsFilled(LogPath) Then
		LogPath = DefaultLogPath;
	EndIf;
	
	If EnableLogging = Undefined Then
		EnableLogging = True;
	EndIf;
	
EndProcedure

&AtClient
Procedure ApplyLoggingSettings()
	
	EnsureLoggingDefaults();
	
	If Component = Undefined Then
		Return;
	EndIf;
	
	Try
		Component.LoggingEnabled = EnableLogging;
		Component.LogPath = LogPath;
	Except
		ShowMessageBox(, "ConfigureLogging failed: " + ErrorDescription());
	EndTry;
	
EndProcedure

&AtClient
Procedure BuildCombinedStatusJSON(Callback)
	
	EnsureRuntimeStatus();
	
	StatusPayload = New Structure;
	StatusPayload.Insert("runtimeStatus", RuntimeStatus);
	
	If Component <> Undefined Then
		Context = New Structure("Callback,StatusPayload", Callback, StatusPayload);
		Try
			StatusJson = Component.Status;
		Except
			StatusJson = "";
		EndTry;
		BuildCombinedStatusEnd(StatusJson, Context);
	Else
		StatusPayload.Insert("componentStatus", New Structure("running", False));
		ExecuteNotifyProcessing(Callback, SerializeToJson(StatusPayload));
	EndIf;
	
EndProcedure

&AtClient
Procedure BuildCombinedStatusEnd(ResultCall, AdditionalParameters) Export
	
	AdditionalParameters.StatusPayload.Insert("componentStatus",
		ParseJsonArgument(ResultCall, ResultCall));
	ExecuteNotifyProcessing(AdditionalParameters.Callback, SerializeToJson(AdditionalParameters.StatusPayload));
	
EndProcedure

&AtServer
Function GetDefaultAddInSource()
	
	Obj = FormAttributeToValue("Object");
	Tmp = Obj.GetTemplate("http1c");
	Addr = PutToTempStorage(Tmp, UUID);
	Return Addr;

EndFunction

&AtClient
Function ExtCompTRegistryDir()
	// Filesystem directory the launcher populates with registry.xml +
	// libhttp1cWin.dll + rcore.dll + DirectML.dll. Attaching a native component
	// from this path makes 1C load it in-place, so rcore.dll (loaded at runtime
	// by the component) is found beside it and real search is enabled. Mirrors
	// Vanessa Automation's silent registry.xml technique without any install modal.
	Return "D:\GitHub\MCP-DB-Client\rust-core\target\extcomp";
EndFunction

#EndRegion


// ============================================================================
// MCP TRANSPORT
// ============================================================================
//
// Low-level methods for sending responses back to the native component.
// These wrap the JSON-RPC response format required by MCP.
//
// For tools:
//   { "content": [{"type": "text", "text": "..."}], "isError": false }
// For resources:
//   { "contents": [{"uri": "...", "mimeType": "...", "text": "..."}] }
// For prompts:
//   { "messages": [{"role": "user", "content": {"type": "text", "text": "..."}}] }
// ============================================================================

#Region MCPTransport

// ---- Tool transport ----

&AtClient
Procedure SendToolResult(RequestID, Text)
	
	If Component = Undefined Then
		Return;
	EndIf;
	
	SendMCPResponse(RequestID, NewMCPContent(Text, False));
	
EndProcedure

&AtClient
Procedure SendToolError(RequestID, ErrorText)
	
	If Component = Undefined Then
		Return;
	EndIf;
	
	SendMCPResponse(RequestID, NewMCPContent(ErrorText, True));
	
EndProcedure

&AtClient
Procedure SendToolProgress(RequestID, Progress, Total, Message)
	
	If Component = Undefined Then
		Return;
	EndIf;
	
	Component.BeginCallingSendProgress(
		New NotifyDescription("EmptyCallbackHandler", ThisObject),
		RequestID, Progress, Total, Message);
	
EndProcedure

// ---- Resource transport ----

&AtClient
Procedure SendResourceResult(RequestID, ResourceResult)
	
	If Component = Undefined Then
		Return;
	EndIf;
	
	SendMCPResponse(RequestID, ResourceResult);
	
EndProcedure

&AtClient
Procedure SendResourceError(RequestID, ErrorText)
	
	If Component = Undefined Then
		Return;
	EndIf;
	
	ErrorResult = New Structure;
	ErrorResult.Insert("error", ErrorText);
	SendMCPResponse(RequestID, ErrorResult);
	
EndProcedure

// ---- Prompt transport ----

&AtClient
Procedure SendPromptResult(RequestID, PromptResult)
	
	If Component = Undefined Then
		Return;
	EndIf;
	
	SendMCPResponse(RequestID, PromptResult);
	
EndProcedure

&AtClient
Procedure SendPromptError(RequestID, ErrorText)
	
	If Component = Undefined Then
		Return;
	EndIf;
	
	ErrorResult = New Structure;
	ErrorResult.Insert("error", ErrorText);
	SendMCPResponse(RequestID, ErrorResult);
	
EndProcedure

// ---- Common ----

&AtClient
Procedure SendMCPResponse(RequestID, ResultStructure)
	
	Response = New Structure;
	Response.Insert("id", RequestID);
	Response.Insert("body", SerializeToJson(ResultStructure));
	
	Component.BeginCallingSendResponse(
		New NotifyDescription("EmptyCallbackHandler", ThisObject),
		SerializeToJson(Response));
	
EndProcedure

&AtClient
Function NewMCPContent(Text, IsError = False)
	
	ContentItem = New Structure;
	ContentItem.Insert("type", "text");
	ContentItem.Insert("text", Text);
	
	Content = New Array;
	Content.Add(ContentItem);
	
	Result = New Structure;
	Result.Insert("content", Content);
	
	If IsError Then
		Result.Insert("isError", True);
	EndIf;
	
	Return Result;
	
EndFunction

#EndRegion


#Region LegacyHTTP

&AtClient
Procedure ProcessLegacyRequest(Data)
	
	Try
		JSONReader = New JSONReader;
		JSONReader.SetString(Data);
		Request = ReadJSON(JSONReader, True);
	Except
		Return;
	EndTry;
	
	RequestID = Request["id"];
	SendHTTPResponse(RequestID, 404,
		New Structure("error", "Use POST /mcp for MCP requests."));
	
EndProcedure

&AtClient
Procedure SendHTTPResponse(ID, Status, ResponseData, ContentType = "application/json")
	
	If Component = Undefined Then
		Return;
	EndIf;
	
	Response = New Structure;
	Response.Insert("id", ID);
	Response.Insert("status", Status);
	Response.Insert("content_type", ContentType);
	Response.Insert("body", SerializeToJson(ResponseData));
	
	Component.BeginCallingSendResponse(
		New NotifyDescription("EmptyCallbackHandler", ThisObject),
		SerializeToJson(Response));
	
EndProcedure

#EndRegion


// ============================================================================
// RAG / SEARCH DEMO
// ============================================================================
//
// Drives the Rust search core (rcore.dll) from 1C via the component's
// RagDispatch(method, payloadJson) method. Ingest/admin calls (configure,
// index_segments, stats, search, delete_collection) are issued from here; the
// search/grep/get_segment MCP tools are served by the component itself (no 1C
// round-trip), so once data is indexed an MCP client can search it too.
//
// End-to-end test flow:
//   1. Connect              - load the native component.
//   2. Configure RAG        - select the e5 model (needs the FULL package + a
//                             staged model; the lite component answers
//                             rag_not_installed).
//   3. Index demo data      - this base's catalog + document metadata as segments.
//   4. Stats                - watch vector_status go building -> ready.
//   5. Search               - dense / keyword / hybrid.
// ============================================================================

#Region RAGDemo

&AtClient
Procedure EnsureRagDefaults()

	If Port = 0 Then
		Port = 8888;
	EndIf;
	If Not ValueIsFilled(RagModel) Then
		// Builtin model name (no hardcoded path). For an offline/local model put a
		// directory path here; the headless self-test takes its model_path from the
		// launch config instead.
		RagModel = "multilingual-e5-small";
	EndIf;
	If Not ValueIsFilled(RagDevice) Then
		RagDevice = "auto";
	EndIf;
	If Not ValueIsFilled(RagCollection) Then
		RagCollection = "metadata";
	EndIf;
	If Not ValueIsFilled(RagMode) Then
		RagMode = "hybrid";
	EndIf;

EndProcedure

// ---- Command handlers ----

&AtClient
Procedure RagConfigure(Command)

	EnsureRagDefaults();

	Payload = New Structure;
	If RagModelLooksLikePath(RagModel) Then
		Payload.Insert("model_path", RagModel);
	Else
		Payload.Insert("model", RagModel);
	EndIf;
	Payload.Insert("device", RagDevice);

	RagCall("configure", SerializeToJson(Payload), "RagConfigureEnd");

EndProcedure

&AtClient
Procedure RagConfigureEnd(ResultJson, ParametersCall, AdditionalParameters) Export

	ShowRagResult("configure", ResultJson);

EndProcedure

&AtClient
Procedure RagIndexDemo(Command)

	EnsureRagDefaults();

	PayloadJson = BuildMetadataSegmentsJSON(RagCollection);
	RagCall("index_segments", PayloadJson, "RagIndexDemoEnd");

EndProcedure

&AtClient
Procedure RagIndexDemoEnd(ResultJson, ParametersCall, AdditionalParameters) Export

	ShowRagResult("index_segments", ResultJson);
	// Poll embedding progress and show a progress indicator in the form until the
	// background worker has embedded every segment (vector_status = ready).
	AttachIdleHandler("RagIndexPollTick", 1, True);

EndProcedure

&AtClient
Procedure RagIndexPollTick() Export

	RagCall("stats", "{}", "RagIndexPollEnd");

EndProcedure

&AtClient
Procedure RagIndexPollEnd(ResultJson, ParametersCall, AdditionalParameters) Export

	Emb = 0;
	Total = 0;
	VecStatus = "";
	Try
		R = New JSONReader;
		R.SetString(ResultJson);
		Obj = ReadJSON(R, True);
		Coll = Obj["result"]["collections"][RagCollection];
		If Coll <> Undefined Then
			Emb = Coll["embedded"];
			Total = Coll["n_segments"];
			VecStatus = Coll["vector_status"];
		EndIf;
	Except
	EndTry;

	ShowEmbedProgress("Эмбеддинг векторов: " + RagCollection, Emb, Total);

	If VecStatus <> "ready" Then
		AttachIdleHandler("RagIndexPollTick", 1, True);
	Else
		Status(); // clear the progress indicator
		RagOutput = "Готово: эмбеддинг " + String(Emb) + " из " + String(Total) + " (100%)";
	EndIf;

EndProcedure

&AtClient
Procedure RagStats(Command)

	RagCall("stats", "{}", "RagStatsEnd");

EndProcedure

// "Synchronize with the vector store": index the steps file + every corpus
// source row (real embedding) into named collections, each carrying a
// description (so an AI client can later list collections + descriptions and
// search a subset). Async (this config forbids sync component calls). Paths are
// taken from the form — nothing is prefilled or auto-loaded.
&AtClient
Procedure SyncVector(Command)

	If Component = Undefined Then
		RagOutput = "Компонента не подключена. Нажмите Connect (нужна полная компонента с rcore.dll).";
		Return;
	EndIf;

	Groups = BuildSyncGroups();
	Total = 0;
	Names = New Array;
	For Each G In Groups Do
		Total = Total + G.segments.Count();
		Names.Add(G.collection);
	EndDo;
	If Total = 0 Then
		RagOutput = "Нечего синхронизировать: укажите путь к шагам и/или строки таблицы корпуса (с существующими папками).";
		Return;
	EndIf;

	SyncCtx = New Structure;
	SyncCtx.Insert("Groups", Groups);
	SyncCtx.Insert("GIndex", 0);
	SyncCtx.Insert("Pos", 0);
	SyncCtx.Insert("Batch", 500);
	SyncCtx.Insert("Total", Total);
	SyncCtx.Insert("CollNames", Names);
	SyncCtx.Insert("T0", Ms());

	RagOutput = "Синхронизация: " + String(Total) + " сегментов → " + String(Groups.Count())
		+ " коллекций (" + StrConcat(Names, ", ") + "). Конфигурирую модель...";

	Cfg = New Structure;
	Cfg.Insert("model_path", RagModel);
	Cfg.Insert("device", ?(ValueIsFilled(RagDevice), RagDevice, "auto"));
	Cfg.Insert("embed_workers", 1);
	Try
		Component.BeginCallingRagDispatch(
			New NotifyDescription("Sync_ConfigureEnd", ThisObject), "configure", SerializeToJson(Cfg));
	Except
		RagOutput = "FAIL configure dispatch: " + ErrorDescription();
	EndTry;

EndProcedure

&AtClient
Procedure Sync_ConfigureEnd(ResultJson, ParametersCall, AdditionalParameters) Export

	If StrFind(ResultJson, """ok"":true") = 0 Then
		RagOutput = "FAIL configure (нужна полная компонента с rcore.dll): " + Left(ResultJson, 300);
		Return;
	EndIf;
	SyncCtx.T0 = Ms();
	Sync_SubmitNext();

EndProcedure

&AtClient
Procedure Sync_SubmitNext()

	// Skip past any finished groups.
	While SyncCtx.GIndex < SyncCtx.Groups.Count() Do
		G = SyncCtx.Groups[SyncCtx.GIndex];
		If SyncCtx.Pos < G.segments.Count() Then
			Break;
		EndIf;
		SyncCtx.GIndex = SyncCtx.GIndex + 1;
		SyncCtx.Pos = 0;
	EndDo;

	If SyncCtx.GIndex >= SyncCtx.Groups.Count() Then
		SelfTestWaitTicks = 0;
		AttachIdleHandler("Sync_Tick", 1, True);
		Return;
	EndIf;

	G = SyncCtx.Groups[SyncCtx.GIndex];
	Upper = SyncCtx.Pos + SyncCtx.Batch - 1;
	If Upper > G.segments.Count() - 1 Then
		Upper = G.segments.Count() - 1;
	EndIf;
	Slice = New Array;
	For i = SyncCtx.Pos To Upper Do
		Slice.Add(G.segments[i]);
	EndDo;
	SyncCtx.Pos = Upper + 1;

	// The collection's description rides on the doc "name" (and is what a later
	// list-collections surfaces to an AI client).
	DocId = G.collection + "-batch-" + String(SyncCtx.Pos);
	Payload = SegmentsPayload(G.collection, DocId, G.description, Slice);
	Try
		Component.BeginCallingRagDispatch(
			New NotifyDescription("Sync_BatchEnd", ThisObject), "index_segments", Payload);
	Except
		RagOutput = "FAIL index dispatch: " + ErrorDescription();
	EndTry;

EndProcedure

&AtClient
Procedure Sync_BatchEnd(ResultJson, ParametersCall, AdditionalParameters) Export

	Sync_SubmitNext();

EndProcedure

&AtClient
Procedure Sync_Tick() Export

	Component.BeginCallingRagDispatch(
		New NotifyDescription("Sync_StatsEnd", ThisObject), "stats", "{}");

EndProcedure

&AtClient
Procedure Sync_StatsEnd(ResultJson, ParametersCall, AdditionalParameters) Export

	SelfTestWaitTicks = SelfTestWaitTicks + 1;
	EmbDone = 0;
	AllReady = True;
	Lines = New Array;
	Try
		R = New JSONReader;
		R.SetString(ResultJson);
		Obj = ReadJSON(R, True);
		Colls = Obj["result"]["collections"];
		For Each Name In SyncCtx.CollNames Do
			C = ?(Colls = Undefined, Undefined, Colls[Name]);
			If C = Undefined Then
				AllReady = False;
				Continue;
			EndIf;
			Emb = C["embedded"];
			NSeg = C["n_segments"];
			St = C["vector_status"];
			EmbDone = EmbDone + Emb;
			If St <> "ready" Then
				AllReady = False;
			EndIf;
			Lines.Add("  " + Name + ": " + String(Emb) + "/" + String(NSeg) + " (" + St + ")");
		EndDo;
	Except
	EndTry;

	ShowEmbedProgress("Эмбеддинг корпуса", EmbDone, SyncCtx.Total);
	Head = "Синхронизация (" + String(Ms() - SyncCtx.T0) + " ms):";
	RagOutput = Head + Chars.LF + StrConcat(Lines, Chars.LF);

	If Not AllReady And SelfTestWaitTicks < 6000 Then
		AttachIdleHandler("Sync_Tick", 1, True);
		Return;
	EndIf;

	RagOutput = RagOutput + Chars.LF + "ГОТОВО за " + String(Ms() - SyncCtx.T0)
		+ " ms — коллекции готовы к поиску.";

EndProcedure

// Build the list of {collection, description, segments[]} groups from the form:
// the steps file (one "steps" collection) + each corpus-source row (one
// collection, or one per subfolder when "By subfolders" is set).
&AtClient
Function BuildSyncGroups()

	Groups = New Array;

	If ValueIsFilled(StepsPath) Then
		StepSegs = ReadVanessaSteps(StepsPath);
		If StepSegs.Count() > 0 Then
			Groups.Add(New Structure("collection, description, segments",
				"steps", "Шаги Vanessa — определения шагов Gherkin (ИмяШага/ОписаниеШага)", StepSegs));
		EndIf;
	EndIf;

	For Each Row In CorpusSources Do
		If Not ValueIsFilled(Row.Path) Then
			Continue;
		EndIf;
		Coll = ?(ValueIsFilled(Row.Collection), Row.Collection, "corpus");
		If Row.BySubfolders Then
			For Each Sub In FindSubfolders(Row.Path) Do
				Segs = ReadFeatureScenarios(Sub.FullName);
				If Segs.Count() > 0 Then
					Descr = ?(ValueIsFilled(Row.Description), Row.Description + " / " + Sub.Name, Sub.Name);
					Groups.Add(New Structure("collection, description, segments",
						Coll + "_" + Sub.Name, Descr, Segs));
				EndIf;
			EndDo;
		Else
			Segs = ReadFeatureScenarios(Row.Path);
			If Segs.Count() > 0 Then
				Groups.Add(New Structure("collection, description, segments", Coll, Row.Description, Segs));
			EndIf;
		EndIf;
	EndDo;

	Return Groups;

EndFunction

// Read Vanessa step definitions from a steps.json array
// [{ИмяШага, ОписаниеШага, ПолныйТипШага}, ...] into segment structures.
&AtClient
Function ReadVanessaSteps(StepsFile)

	Segments = New Array;
	Data = Undefined;
	Try
		F = New File(StepsFile);
		If Not F.Exists() Then
			Return Segments;
		EndIf;
		R = New JSONReader;
		R.OpenFile(StepsFile);
		Data = ReadJSON(R, True);
		R.Close();
	Except
		Return Segments;
	EndTry;
	If TypeOf(Data) <> Type("Array") Then
		Return Segments;
	EndIf;
	For Each St In Data Do
		Name = GetArg(St, "ИмяШага");
		Descr = GetArg(St, "ОписаниеШага");
		StepType = GetArg(St, "ПолныйТипШага");
		If Not ValueIsFilled(Name) Then
			Continue;
		EndIf;
		Seg = New Structure;
		Seg.Insert("text", Name);
		Seg.Insert("embed_text", ?(ValueIsFilled(Descr), Name + " " + Descr, Name));
		Seg.Insert("meta", New Structure("type, stepType", "step", String(StepType)));
		Segments.Add(Seg);
	EndDo;
	Return Segments;

EndFunction

&AtClient
Function FindSubfolders(Dir)

	Result = New Array;
	Try
		For Each F In FindFiles(Dir, "*", False) Do
			If F.IsDirectory() Then
				Result.Add(F);
			EndIf;
		EndDo;
	Except
	EndTry;
	Return Result;

EndFunction

// Dev-only headless test of the Sync button: prefill the form fields from the
// launch config and fire SyncVector, then mirror RagOutput to the result file so
// the headless runner can observe progress + DONE.
&AtClient
Procedure Sync_TestTrigger() Export

	StepsPath = SelfTestCfg.steps;
	If ValueIsFilled(SelfTestCfg.corpus) Then
		Row = CorpusSources.Add();
		Row.Path = SelfTestCfg.corpus;
		Row.Collection = ?(ValueIsFilled(SelfTestCfg.coll), SelfTestCfg.coll, "corpus");
		Row.Description = "synctest corpus";
		Row.BySubfolders = False;
	EndIf;
	RagModel = SelfTestCfg.model;
	SyncVector(Undefined);
	AttachIdleHandler("Sync_TestDump", 2, True);

EndProcedure

&AtClient
Procedure Sync_TestDump() Export

	Try
		W = New TextWriter(SelfTestOutFile("ragselftest-result.txt"), TextEncoding.UTF8, Chars.LF, False);
		W.WriteLine("build=" + BuildVersion() + " SYNCTEST");
		W.WriteLine(RagOutput);
		If StrFind(RagOutput, "ГОТОВО") > 0 Or StrFind(RagOutput, "FAIL") > 0 Then
			W.WriteLine("DONE");
			W.Close();
			Return;
		EndIf;
		W.Close();
	Except
	EndTry;
	AttachIdleHandler("Sync_TestDump", 2, True);

EndProcedure

&AtClient
Procedure RagStatsEnd(ResultJson, ParametersCall, AdditionalParameters) Export

	ShowRagResult("stats", ResultJson);

EndProcedure

&AtClient
Procedure RagSearch(Command)

	EnsureRagDefaults();

	If Not ValueIsFilled(RagQuery) Then
		ShowMessageBox(, "Enter a search query first.");
		Return;
	EndIf;

	Payload = New Structure;
	Payload.Insert("query", RagQuery);
	Payload.Insert("collection", RagCollection);
	Payload.Insert("mode", RagMode);
	Payload.Insert("k", 10);
	Payload.Insert("include_text", True);

	RagCall("search", SerializeToJson(Payload), "RagSearchEnd");

EndProcedure

&AtClient
Procedure RagSearchEnd(ResultJson, ParametersCall, AdditionalParameters) Export

	ShowRagResult("search", ResultJson);

EndProcedure

&AtClient
Procedure RagClear(Command)

	EnsureRagDefaults();

	Payload = New Structure("collection", RagCollection);
	RagCall("delete_collection", SerializeToJson(Payload), "RagClearEnd");

EndProcedure

&AtClient
Procedure RagClearEnd(ResultJson, ParametersCall, AdditionalParameters) Export

	ShowRagResult("delete_collection", ResultJson);

EndProcedure

// ---- Helpers ----

&AtClient
Function RagModelLooksLikePath(Value)

	Return StrFind(Value, "\") > 0 Or StrFind(Value, "/") > 0 Or StrFind(Value, ":") > 0;

EndFunction

&AtClient
Procedure RagCall(Method, PayloadJson, CallbackName)

	If Component = Undefined Then
		ShowMessageBox(, "Component is not connected. Press Connect first.");
		Return;
	EndIf;

	Try
		Component.BeginCallingRagDispatch(
			New NotifyDescription(CallbackName, ThisObject),
			Method, PayloadJson);
	Except
		ShowMessageBox(, "RagDispatch('" + Method + "') failed: " + ErrorDescription());
	EndTry;

EndProcedure

&AtClient
Procedure ShowEmbedProgress(Stage, Emb, Total)

	Pct = ?(Total > 0, Int(Emb * 100 / Total), 0);
	Descr = String(Emb) + " из " + String(Total) + " (" + String(Pct) + "%)";
	// Native 1C progress indicator: message + 0..100 bar + explanatory text.
	Status(Stage, Pct, Descr);
	// Also mirror into the visible output field on the form.
	RagOutput = Stage + ": " + Descr;

EndProcedure

&AtClient
Procedure ShowRagResult(Label, ResultJson)

	Envelope = ParseJsonArgument(ResultJson, Undefined);

	Lines = New Array;
	Lines.Add("=== " + Label + " ===");

	If Envelope = Undefined Then
		Lines.Add("(unparseable response)");
		Lines.Add(Left(ResultJson, 4000));
		RagOutput = JoinRagLines(Lines);
		Return;
	EndIf;

	If GetArg(Envelope, "ok") = True Then
		ResultValue = GetArg(Envelope, "result");
		Hits = Undefined;
		If TypeOf(ResultValue) = Type("Map") Then
			Hits = ResultValue["hits"];
		EndIf;

		If Hits <> Undefined Then
			Lines.Add("hits: " + String(Hits.Count()));
			For Each Hit In Hits Do
				DocId = GetArg(Hit, "doc_id");
				Score = GetArg(Hit, "score");
				Text = GetArg(Hit, "text");
				Snippet = "";
				If Text <> Undefined Then
					Snippet = Left(StrReplace(String(Text), Chars.LF, " "), 100);
				EndIf;
				ScoreStr = ?(Score = Undefined, "", Format(Score, "NFD=4; NG=0"));
				Lines.Add("  [" + ScoreStr + "] " + String(DocId) + " — " + Snippet);
			EndDo;
		Else
			Lines.Add("OK: " + SerializeToJson(ResultValue));
		EndIf;
	Else
		ErrorObj = GetArg(Envelope, "error");
		Lines.Add("ERROR [" + String(GetArg(ErrorObj, "code")) + "]: "
			+ String(GetArg(ErrorObj, "message")));
	EndIf;

	RagOutput = JoinRagLines(Lines);

EndProcedure

&AtClient
Function JoinRagLines(Lines)

	// StrConcat over the ready array instead of +-in-a-loop (O(N) vs O(N^2)).
	If Lines.Count() = 0 Then
		Return "";
	EndIf;
	Return StrConcat(Lines, Chars.LF) + Chars.LF;

EndFunction

&AtServer
Function BuildMetadataSegmentsJSON(Collection)

	// Index this base's catalog + document metadata as searchable segments
	// (a single doc_id "metadata" with many segments). Capped so a very large
	// configuration does not build an enormous payload for a demo.
	MaxSegments = 500;

	Segments = New Array;

	For Each Cat In Metadata.Catalogs Do
		If Segments.Count() >= MaxSegments Then
			Break;
		EndIf;
		Segments.Add(New Structure("text",
			"Справочник " + Cat.Name + " — " + String(Cat.Synonym)));
	EndDo;

	For Each Doc In Metadata.Documents Do
		If Segments.Count() >= MaxSegments Then
			Break;
		EndIf;
		Segments.Add(New Structure("text",
			"Документ " + Doc.Name + " — " + String(Doc.Synonym)));
	EndDo;

	Payload = New Structure;
	Payload.Insert("collection", Collection);
	Payload.Insert("doc_id", "metadata");
	Payload.Insert("name", "1C metadata (catalogs + documents)");
	Payload.Insert("segments", Segments);

	JSONWriter = New JSONWriter;
	JSONWriter.SetString();
	WriteJSON(JSONWriter, Payload);
	Return JSONWriter.Close();

EndFunction

#EndRegion


#Region RAGSelfTest

// Headless self-test driven by the /C"ragselftest" launch parameter (see OnOpen).
// Synchronously attaches the component and round-trips RagDispatch, writing the
// envelopes to rust-core/target/ragselftest-result.txt. The launcher reads that
// file and terminates the 1C process it itself started (by its own PID).

// Async full-chain self-test (this config forbids synchronous component calls).
// attach-only: the component is already cached in ExtCompT (the launcher stages
// libhttp1cWin.dll + rcore.dll + DirectML.dll there), so no InstallAddIn is
// needed and rcore.dll loaded beside the component enables real search.
// Flow: attach -> configure(local e5 model) -> index steps.json -> wait for
// embedding -> hybrid search. Each step is logged; the launcher waits for "DONE".

&AtClient
Procedure RunRagSelfTestDeferred()

	Ctx = New Structure;
	Ctx.Insert("Log", New Array);
	Ctx.Insert("CaseIndex", 0);
	Ctx.Insert("Pass", 0);
	Ctx.Insert("Fail", 0);
	SelfTestAppend(Ctx, "STARTED " + String(CurrentDate()) + " build=" + BuildVersion());

	If Component = Undefined Then
		SelfTestAppend(Ctx, "FAIL: component not attached on open");
		SelfTestAppend(Ctx, "RESULT: FAIL (0/0)");
		SelfTestAppend(Ctx, "DONE");
		Return;
	EndIf;
	Try
		SelfTestAppend(Ctx, "component version = " + Component.Version);
	Except
	EndTry;

	// Build the stub-adapter test cases (catch any builder error instead of
	// letting an idle-handler exception pop a blocking modal).
	Try
		Ctx.Insert("Cases", SelfTestCases());
	Except
		SelfTestAppend(Ctx, "FAIL: building test cases -> " + ErrorDescription());
		SelfTestAppend(Ctx, "RESULT: FAIL (0/0)");
		SelfTestAppend(Ctx, "DONE");
		Return;
	EndTry;

	// Offline model + device come from the launch config (no hardcoded paths).
	// device "auto" = DirectML GPU with automatic CPU fallback.
	Cfg = New Structure;
	Cfg.Insert("model_path", SelfTestCfg.model);
	Cfg.Insert("device", "auto");
	Ctx.Insert("TCfg", Ms());
	Try
		Component.BeginCallingRagDispatch(
			New NotifyDescription("SelfTest_ConfigureEnd", ThisObject, Ctx),
			"configure", SerializeToJson(Cfg));
	Except
		SelfTestAppend(Ctx, "FAIL: configure dispatch -> " + ErrorDescription());
		SelfTestAppend(Ctx, "RESULT: FAIL (0/" + String(Ctx.Cases.Count()) + ")");
		SelfTestAppend(Ctx, "DONE");
	EndTry;

EndProcedure

&AtClient
Procedure SelfTest_ConfigureEnd(ResultJson, ParametersCall, Ctx) Export

	SelfTestAppend(Ctx, "configure (" + String(Ms() - Ctx.TCfg) + " ms) -> " + Left(ResultJson, 240));
	If StrFind(ResultJson, """ok"":true") = 0 Then
		SelfTestAppend(Ctx, "FAIL: configure — real embedder unavailable (rag_not_installed / mock)");
		SelfTestAppend(Ctx, "RESULT: FAIL (0/" + String(Ctx.Cases.Count()) + ")");
		SelfTestAppend(Ctx, "DONE");
		Return;
	EndIf;
	If StrFind(ResultJson, """dim"":384") = 0 Then
		SelfTestAppend(Ctx, "WARN: model dim != 384 (mock fallback?) — semantic asserts may fail");
	EndIf;
	SelfTest_RunCase(Ctx);

EndProcedure

// --- Generic asserting driver: each case is index -> poll-ready -> query -> assert ---

&AtClient
Procedure SelfTest_RunCase(Ctx)

	If Ctx.CaseIndex >= Ctx.Cases.Count() Then
		SelfTest_Finalize(Ctx);
		Return;
	EndIf;
	C = Ctx.Cases[Ctx.CaseIndex];
	Ctx.Insert("CurCase", C);
	Ctx.Insert("CaseSegments", 0);
	SelfTestAppend(Ctx, "");
	SelfTestAppend(Ctx, "CASE " + String(Ctx.CaseIndex + 1) + "/" + String(Ctx.Cases.Count())
		+ ": " + C.label + " (collection " + C.collection + ")");

	If Not ValueIsFilled(C.indexMethod) Then
		// No (re)index — query an already-indexed collection directly (find_step_usages).
		Ctx.Insert("TQuery", Ms());
		Component.BeginCallingRagDispatch(
			New NotifyDescription("SelfTest_CaseQueryEnd", ThisObject, Ctx),
			C.queryMethod, C.queryPayload);
		Return;
	EndIf;

	Ctx.Insert("TEmbed", Ms());
	Component.BeginCallingRagDispatch(
		New NotifyDescription("SelfTest_CaseIndexEnd", ThisObject, Ctx),
		C.indexMethod, C.indexPayload);

EndProcedure

&AtClient
Procedure SelfTest_CaseIndexEnd(ResultJson, ParametersCall, Ctx) Export

	SelfTestAppend(Ctx, "  " + Ctx.CurCase.indexMethod + " -> " + Left(ResultJson, 160));
	SelfTestCtx = Ctx;
	SelfTestWaitTicks = 0;
	AttachIdleHandler("SelfTest_CaseTick", 5, True);

EndProcedure

&AtClient
Procedure SelfTest_CaseTick() Export

	Component.BeginCallingRagDispatch(
		New NotifyDescription("SelfTest_CaseStatsEnd", ThisObject, SelfTestCtx), "stats", "{}");

EndProcedure

&AtClient
Procedure SelfTest_CaseStatsEnd(ResultJson, ParametersCall, Ctx) Export

	SelfTestWaitTicks = SelfTestWaitTicks + 1;
	C = Ctx.CurCase;
	Emb = 0;
	Total = 0;
	VecStatus = "";
	Try
		R = New JSONReader;
		R.SetString(ResultJson);
		Obj = ReadJSON(R, True);
		Coll = Obj["result"]["collections"][C.collection];
		If Coll <> Undefined Then
			Emb = Coll["embedded"];
			Total = Coll["n_segments"];
			VecStatus = Coll["vector_status"];
		EndIf;
	Except
	EndTry;
	Ctx.CaseSegments = Total;
	ShowEmbedProgress(C.label, Emb, Total);

	If VecStatus <> "ready" And SelfTestWaitTicks < 60 Then
		AttachIdleHandler("SelfTest_CaseTick", 5, True);
		Return;
	EndIf;

	SelfTestAppend(Ctx, "  embedded " + String(Emb) + "/" + String(Total)
		+ " in " + String(Ms() - Ctx.TEmbed) + " ms; querying");
	Ctx.Insert("TQuery", Ms());
	Component.BeginCallingRagDispatch(
		New NotifyDescription("SelfTest_CaseQueryEnd", ThisObject, Ctx),
		C.queryMethod, C.queryPayload);

EndProcedure

&AtClient
Procedure SelfTest_CaseQueryEnd(ResultJson, ParametersCall, Ctx) Export

	C = Ctx.CurCase;
	SelfTestAppend(Ctx, "  " + C.queryMethod + " (" + String(Ms() - Ctx.TQuery) + " ms) -> "
		+ Left(ResultJson, 400));

	OkPass = (StrFind(ResultJson, """ok"":true") > 0);
	TextPass = (Not ValueIsFilled(C.expectText)) Or (StrFind(ResultJson, C.expectText) > 0);
	SegPass = (C.expectSegments = 0) Or (Ctx.CaseSegments = C.expectSegments);
	CasePass = OkPass And TextPass And SegPass;

	Detail = "ok=" + String(OkPass);
	If ValueIsFilled(C.expectText) Then
		Detail = Detail + " text<" + C.expectText + ">=" + String(TextPass);
	EndIf;
	If C.expectSegments > 0 Then
		Detail = Detail + " segments(" + String(Ctx.CaseSegments) + "==" + String(C.expectSegments)
			+ ")=" + String(SegPass);
	EndIf;

	If CasePass Then
		Ctx.Pass = Ctx.Pass + 1;
		SelfTestAppend(Ctx, "  PASS: " + C.label + " [" + Detail + "]");
	Else
		Ctx.Fail = Ctx.Fail + 1;
		SelfTestAppend(Ctx, "  FAIL: " + C.label + " [" + Detail + "]");
	EndIf;

	Ctx.CaseIndex = Ctx.CaseIndex + 1;
	SelfTest_RunCase(Ctx);

EndProcedure

&AtClient
Procedure SelfTest_Finalize(Ctx)

	Total = Ctx.Cases.Count();
	SelfTestAppend(Ctx, "");
	If Ctx.Fail = 0 Then
		SelfTestAppend(Ctx, "RESULT: ALL PASS (" + String(Ctx.Pass) + "/" + String(Total) + ")");
	Else
		SelfTestAppend(Ctx, "RESULT: FAIL (" + String(Ctx.Pass) + "/" + String(Total)
			+ " passed, " + String(Ctx.Fail) + " failed)");
	EndIf;
	SelfTestAppend(Ctx, "DONE");

EndProcedure

// ============================================================================
// Embedding benchmark over the whole .feature corpus (launch param
// embedperf=<dir>). Reads every *.feature under <dir>, chunks BY SCENARIO,
// indexes with REAL embedding in batches, polls until the worker has embedded
// everything, and reports the total embedding-build time + throughput.
// ============================================================================

&AtClient
Procedure RunEmbedPerfDeferred()

	EmbedCtx = New Structure;
	EmbedCtx.Insert("Log", New Array);
	SelfTestAppend(EmbedCtx, "STARTED " + String(CurrentDate()) + " build=" + BuildVersion() + " EMBEDPERF");
	If Component = Undefined Then
		SelfTestAppend(EmbedCtx, "FAIL: component not attached on open");
		SelfTestAppend(EmbedCtx, "DONE");
		Return;
	EndIf;
	Try
		SelfTestAppend(EmbedCtx, "component version = " + Component.Version);
	Except
	EndTry;

	Cfg = New Structure;
	Cfg.Insert("model_path", SelfTestCfg.model);
	Cfg.Insert("device", "auto");
	Cfg.Insert("embed_workers", SelfTestCfg.workers);
	SelfTestAppend(EmbedCtx, "embed_workers requested = " + String(SelfTestCfg.workers)
		+ " (0=auto~ncpu/2, 1=single, N=exact)");
	EmbedCtx.Insert("TCfg", Ms());
	Try
		Component.BeginCallingRagDispatch(
			New NotifyDescription("EmbedPerf_ConfigureEnd", ThisObject),
			"configure", SerializeToJson(Cfg));
	Except
		SelfTestAppend(EmbedCtx, "FAIL: configure dispatch -> " + ErrorDescription());
		SelfTestAppend(EmbedCtx, "DONE");
	EndTry;

EndProcedure

&AtClient
Procedure EmbedPerf_ConfigureEnd(ResultJson, ParametersCall, AdditionalParameters) Export

	SelfTestAppend(EmbedCtx, "configure (" + String(Ms() - EmbedCtx.TCfg) + " ms) -> " + Left(ResultJson, 240));
	If StrFind(ResultJson, """ok"":true") = 0 Then
		SelfTestAppend(EmbedCtx, "FAIL: configure -> " + Left(ResultJson, 240));
		SelfTestAppend(EmbedCtx, "DONE");
		Return;
	EndIf;

	TRead = Ms();
	Segments = ReadFeatureScenarios(SelfTestCfg.embedperf);
	EmbedCtx.Insert("Segments", Segments);
	EmbedCtx.Insert("Total", Segments.Count());
	EmbedCtx.Insert("Pos", 0);
	EmbedCtx.Insert("Batch", ?(SelfTestCfg.batch > 0, SelfTestCfg.batch, 500));
	EmbedCtx.Insert("Collection", "features");
	EmbedCtx.Insert("Batches", 0);
	SelfTestAppend(EmbedCtx, "scenarios=" + String(EmbedCtx.Total)
		+ " read+chunk in " + String(Ms() - TRead) + " ms; batch=" + String(EmbedCtx.Batch));

	If EmbedCtx.Total = 0 Then
		SelfTestAppend(EmbedCtx, "FAIL: no scenarios found under " + SelfTestCfg.embedperf);
		SelfTestAppend(EmbedCtx, "DONE");
		Return;
	EndIf;

	EmbedCtx.Insert("T0", Ms());
	EmbedPerf_SubmitNextBatch();

EndProcedure

&AtClient
Procedure EmbedPerf_SubmitNextBatch()

	If EmbedCtx.Pos >= EmbedCtx.Total Then
		SelfTestAppend(EmbedCtx, "submitted all " + String(EmbedCtx.Total) + " segments in "
			+ String(EmbedCtx.Batches) + " batches, accept took " + String(Ms() - EmbedCtx.T0)
			+ " ms; embedding in background...");
		SelfTestWaitTicks = 0;
		AttachIdleHandler("EmbedPerf_Tick", 1, True);
		Return;
	EndIf;

	Upper = EmbedCtx.Pos + EmbedCtx.Batch - 1;
	If Upper > EmbedCtx.Total - 1 Then
		Upper = EmbedCtx.Total - 1;
	EndIf;
	Slice = New Array;
	For i = EmbedCtx.Pos To Upper Do
		Slice.Add(EmbedCtx.Segments[i]);
	EndDo;
	EmbedCtx.Pos = Upper + 1;
	EmbedCtx.Batches = EmbedCtx.Batches + 1;

	Payload = SegmentsPayload(EmbedCtx.Collection, "features-batch-" + String(EmbedCtx.Batches),
		"features batch " + String(EmbedCtx.Batches), Slice);
	Try
		Component.BeginCallingRagDispatch(
			New NotifyDescription("EmbedPerf_BatchEnd", ThisObject),
			"index_segments", Payload);
	Except
		SelfTestAppend(EmbedCtx, "FAIL: index_segments dispatch -> " + ErrorDescription());
		SelfTestAppend(EmbedCtx, "DONE");
	EndTry;

EndProcedure

&AtClient
Procedure EmbedPerf_BatchEnd(ResultJson, ParametersCall, AdditionalParameters) Export

	If StrFind(ResultJson, """ok"":true") = 0 Then
		SelfTestAppend(EmbedCtx, "batch " + String(EmbedCtx.Batches) + " -> " + Left(ResultJson, 200));
	EndIf;
	EmbedPerf_SubmitNextBatch();

EndProcedure

&AtClient
Procedure EmbedPerf_Tick() Export

	Component.BeginCallingRagDispatch(
		New NotifyDescription("EmbedPerf_StatsEnd", ThisObject), "stats", "{}");

EndProcedure

&AtClient
Procedure EmbedPerf_StatsEnd(ResultJson, ParametersCall, AdditionalParameters) Export

	SelfTestWaitTicks = SelfTestWaitTicks + 1;
	Emb = 0;
	Failed = 0;
	Skipped = 0;
	NSeg = 0;
	VecStatus = "";
	Try
		R = New JSONReader;
		R.SetString(ResultJson);
		Obj = ReadJSON(R, True);
		Coll = Obj["result"]["collections"][EmbedCtx.Collection];
		If Coll <> Undefined Then
			Emb = Coll["embedded"];
			Failed = Coll["failed"];
			Skipped = Coll["skipped"];
			NSeg = Coll["n_segments"];
			VecStatus = Coll["vector_status"];
		EndIf;
	Except
	EndTry;

	ShowEmbedProgress("Embedding features corpus", Emb, EmbedCtx.Total);

	Done = (Emb + Failed + Skipped >= EmbedCtx.Total) Or (VecStatus = "ready");
	If Not Done And SelfTestWaitTicks < 6000 Then
		// Periodic progress line (~every 10s) so a long run is observable in the file.
		If (SelfTestWaitTicks % 10) = 0 Then
			SelfTestAppend(EmbedCtx, "  progress: " + String(Emb) + "/" + String(EmbedCtx.Total)
				+ " embedded, " + String(Ms() - EmbedCtx.T0) + " ms elapsed");
		EndIf;
		AttachIdleHandler("EmbedPerf_Tick", 1, True);
		Return;
	EndIf;

	Elapsed = Ms() - EmbedCtx.T0;
	MsPerSeg = ?(Emb > 0, Elapsed / Emb, 0);
	Rate = ?(Elapsed > 0, Int(Emb * 1000 / Elapsed), 0);
	SelfTestAppend(EmbedCtx, "");
	SelfTestAppend(EmbedCtx, "EMBEDDED " + String(Emb) + "/" + String(EmbedCtx.Total)
		+ " (failed " + String(Failed) + ", skipped " + String(Skipped) + ", n_segments " + String(NSeg)
		+ ") in " + String(Elapsed) + " ms");
	SelfTestAppend(EmbedCtx, "throughput = " + String(Rate) + " seg/s, "
		+ String(Int(MsPerSeg * 100) / 100) + " ms/seg; vector_status=" + VecStatus);
	If Not Done Then
		SelfTestAppend(EmbedCtx, "WARN: stopped on tick cap (still building)");
	EndIf;
	SelfTestAppend(EmbedCtx, "RESULT: EMBEDPERF DONE");
	SelfTestAppend(EmbedCtx, "DONE");

EndProcedure

// Read every *.feature under Dir (recursive), chunk each BY SCENARIO, and return
// an array of segment structures {text, embed_text, meta}. embed_text = the
// scenario text so it is REALLY embedded (this is the whole point of the bench).
&AtClient
Function ReadFeatureScenarios(Dir)

	Segments = New Array;
	Files = New Array;
	Try
		Files = FindFiles(Dir, "*.feature", True);
	Except
	EndTry;
	For Each F In Files Do
		If F.IsDirectory() Then
			Continue;
		EndIf;
		Text = "";
		Try
			TR = New TextReader(F.FullName, TextEncoding.UTF8);
			Text = TR.Read();
			TR.Close();
		Except
			Continue;
		EndTry;
		SplitFeatureIntoScenarios(Text, F.Name, Segments);
	EndDo;
	Return Segments;

EndFunction

&AtClient
Function IsScenarioHeader(TrimmedLine)
	Return StrStartsWith(TrimmedLine, "Сценарий:")
		Or StrStartsWith(TrimmedLine, "Scenario:")
		Or StrStartsWith(TrimmedLine, "Scenario Outline:")
		Or StrStartsWith(TrimmedLine, "Структура сценария:");
EndFunction

&AtClient
Procedure SplitFeatureIntoScenarios(Text, FileName, Segments)

	Lines = StrSplit(Text, Chars.LF, True);
	Current = New Array;
	InScenario = False;
	For Each Line In Lines Do
		Clean = StrReplace(Line, Chars.CR, "");
		If IsScenarioHeader(TrimL(Clean)) Then
			If InScenario And Current.Count() > 0 Then
				AddScenarioSegment(Segments, Current, FileName);
			EndIf;
			Current = New Array;
			Current.Add(Clean);
			InScenario = True;
		ElsIf InScenario Then
			Current.Add(Clean);
		EndIf;
	EndDo;
	If InScenario And Current.Count() > 0 Then
		AddScenarioSegment(Segments, Current, FileName);
	EndIf;

EndProcedure

&AtClient
Procedure AddScenarioSegment(Segments, CurrentLines, FileName)

	Body = StrConcat(CurrentLines, Chars.LF);
	Seg = New Structure;
	Seg.Insert("text", Body);
	Seg.Insert("embed_text", Body);
	Seg.Insert("meta", New Structure("type, feature", "scenario", FileName));
	Segments.Add(Seg);

EndProcedure

// ============================================================================
// Test cases — each exercises one adapter end-to-end and asserts the result.
// ============================================================================

&AtClient
Function SelfTestCases()
	Cases = New Array;

	// Perf mode (launch param perf=N): benchmark keyword-search latency over an
	// N-segment synthetic corpus. The first case indexes the corpus then queries;
	// the rest re-query the SAME collection (indexMethod="") so each reports a clean
	// per-query latency (the "search (X ms)" line) without re-indexing. embed_text
	// is blank → segments are skipped by the embedder, isolating the keyword path.
	If SelfTestCfg.perf > 0 Then
		N = SelfTestCfg.perf;
		Cases.Add(SelfTestCase("PERF index+query N=" + String(N), "perf", "index_segments",
			PerfSegmentsPayload("perf", "perf-corpus", N), "search",
			SearchJson("alpha", "perf", "keyword", 10), "alpha", 0));
		For i = 1 To 5 Do
			Cases.Add(SelfTestCase("PERF query #" + String(i + 1) + " N=" + String(N), "perf", "", "", "search",
				SearchJson("alpha", "perf", "keyword", 10), "alpha", 0));
		EndDo;
		Return Cases;
	EndIf;

	// 1. QA step catalog (anti-hallucination source): canonical phrases + descriptions.
	Cases.Add(SelfTestCase("QA step catalog", "qa_steps", "index_segments",
		StepCatalogPayload(), "search",
		SearchJson("удаление пользователя", "qa_steps", "hybrid", 5), "удаля", 0));

	// 2. QA scenarios: verbatim text + tags + line addressing; assert the real
	//    parameter value ("Феррон") is retrievable.
	Cases.Add(SelfTestCase("QA scenarios", "qa_scenarios", "index_segments",
		ScenariosPayload(), "search",
		SearchJson("фильтр по компании", "qa_scenarios", "hybrid", 5), "Феррон", 0));

	// 3. find_step_usages: reverse step -> scenario with the REAL parameter value
	//    (keyword scan over scenario text; qa_scenarios already indexed by case 2).
	Cases.Add(SelfTestCase("find_step_usages", "qa_scenarios", "", "", "search",
		SearchJson("я удаляю пользователя", "qa_scenarios", "keyword", 5), "VanessaUser1", 0));

	// 4. Products adapter: catalog from a stub array; semantic intent (dense).
	Cases.Add(SelfTestCase("Products (semantic)", "products", "index_segments",
		ProductsPayload(), "search",
		SearchJson("ноутбук", "products", "hybrid", 5), "Lenovo", 0));

	// 5. Products by exact article/SKU via the keyword channel (already indexed).
	Cases.Add(SelfTestCase("Products by article (keyword/SKU)", "products", "", "", "search",
		SearchJson("ART-1003", "products", "keyword", 5), "DeLonghi", 0));

	// 6. Clients adapter + dedup by exact INN: raw list has duplicate INNs; assert
	//    the indexed segment count equals the unique count (dedup happened).
	Unique = ClientsDedup(StubClients());
	Cases.Add(SelfTestCase("Clients (dedup by INN)", "clients", "index_segments",
		ClientsPayload(Unique), "search",
		SearchJson("Ромашка", "clients", "keyword", 5), "Ромашка", Unique.Count()));

	Return Cases;
EndFunction

&AtClient
Function SelfTestCase(Label, Collection, IndexMethod, IndexPayload, QueryMethod, QueryPayload, ExpectText, ExpectSegments)
	C = New Structure;
	C.Insert("label", Label);
	C.Insert("collection", Collection);
	C.Insert("indexMethod", IndexMethod);
	C.Insert("indexPayload", IndexPayload);
	C.Insert("queryMethod", QueryMethod);
	C.Insert("queryPayload", QueryPayload);
	C.Insert("expectText", ExpectText);
	C.Insert("expectSegments", ExpectSegments);
	Return C;
EndFunction

&AtClient
Function SearchJson(Query, Collection, Mode, K)
	Sp = New Structure;
	Sp.Insert("query", Query);
	Sp.Insert("collection", Collection);
	Sp.Insert("mode", Mode);
	Sp.Insert("k", K);
	Sp.Insert("include_text", True);
	Return SerializeToJson(Sp);
EndFunction

&AtClient
Function PerfSegmentsPayload(Collection, DocId, Count)
	// N synthetic segments. Every segment contains the common token "alpha" (so a
	// keyword query for it matches ALL N — the worst case for the old build-a-full-
	// Hit-per-match-then-full-sort path) plus a unique token so segments differ.
	// embed_text="" marks each skip → no embedding, isolating the keyword channel.
	Segments = New Array;
	For i = 1 To Count Do
		Seg = New Structure;
		Seg.Insert("text", "alpha beta gamma segment number " + String(i) + " unique" + String(i));
		Seg.Insert("embed_text", "");
		Segments.Add(Seg);
	EndDo;
	Return SegmentsPayload(Collection, DocId, "perf corpus", Segments);
EndFunction

&AtClient
Function SegmentsPayload(Collection, DocId, Name, Segments)
	Payload = New Structure;
	Payload.Insert("collection", Collection);
	Payload.Insert("doc_id", DocId);
	Payload.Insert("name", Name);
	Payload.Insert("segments", Segments);
	W = New JSONWriter;
	W.SetString();
	WriteJSON(W, Payload);
	Return W.Close();
EndFunction

// ============================================================================
// Pluggable stub data sources (no file reads, no hardcoded corpora). Swap these
// for real adapters (Gherkin1C parse, step registry, product/client catalogs).
// ============================================================================

&AtClient
Function StubStep(Phrase, Description, ParamTypes)
	Return New Structure("phrase, description, paramTypes", Phrase, Description, ParamTypes);
EndFunction

&AtClient
Function StubStepCatalog()
	S = New Array;
	S.Add(StubStep("Я удаляю пользователя ""Имя""", "Удаление пользователя информационной базы по имени", "Строка"));
	S.Add(StubStep("Я создаю элемент справочника ""Имя""", "Создание нового элемента справочника", "Строка"));
	S.Add(StubStep("Я открываю форму ""Форма""", "Открытие управляемой формы по имени", "Строка"));
	S.Add(StubStep("Я нажимаю кнопку ""Кнопка""", "Нажатие командной кнопки на форме", "Строка"));
	S.Add(StubStep("Я проверяю фильтр по компании ""Компания""", "Проверка фильтра списка по реквизиту Компания", "Строка"));
	S.Add(StubStep("Я провожу документ ""Документ""", "Проведение документа", "Строка"));
	Return S;
EndFunction

&AtClient
Function StubScenario(Name, Feature, Tags, LineStart, LineEnd, StepsText)
	Return New Structure("name, feature, tags, lineStart, lineEnd, steps",
		Name, Feature, Tags, LineStart, LineEnd, StepsText);
EndFunction

&AtClient
Function StubScenarios()
	S = New Array;
	S.Add(StubScenario("Удаление пользователя", "Управление пользователями", "@smoke,@users", 5, 9,
		"Дано я авторизован как ""Администратор""" + Chars.LF
		+ "Когда я удаляю пользователя ""VanessaUser1""" + Chars.LF
		+ "Тогда пользователь ""VanessaUser1"" отсутствует в базе"));
	S.Add(StubScenario("Фильтр по компании в заказе поставщику", "Фильтры документов", "@filters,@regress", 12, 17,
		"Дано я открываю список ""Заказ поставщику""" + Chars.LF
		+ "Когда я проверяю фильтр по компании ""Феррон""" + Chars.LF
		+ "Тогда в списке только документы компании ""Феррон"""));
	S.Add(StubScenario("Проведение приходной накладной", "Складские документы", "@smoke,@warehouse", 20, 24,
		"Дано открыта форма ""Приходная накладная""" + Chars.LF
		+ "Когда я провожу документ ""ПН-0001""" + Chars.LF
		+ "Тогда документ ""ПН-0001"" проведён"));
	Return S;
EndFunction

&AtClient
Function StubProduct(Name, Brand, Category, Sku, Article, Tags)
	Return New Structure("name, brand, category, sku, article, tags",
		Name, Brand, Category, Sku, Article, Tags);
EndFunction

&AtClient
Function StubProducts()
	P = New Array;
	P.Add(StubProduct("Ноутбук Lenovo ThinkPad X1 Carbon", "Lenovo", "Ноутбуки", "SKU-NB-001", "ART-1001", "электроника,ноутбуки"));
	P.Add(StubProduct("Смартфон Samsung Galaxy S24 Ultra", "Samsung", "Смартфоны", "SKU-SM-002", "ART-1002", "электроника,смартфоны"));
	P.Add(StubProduct("Кофемашина DeLonghi Magnifica", "DeLonghi", "Кухонная техника", "SKU-KM-003", "ART-1003", "техника,кухня"));
	P.Add(StubProduct("Монитор Dell UltraSharp 27", "Dell", "Мониторы", "SKU-MN-004", "ART-1004", "электроника,мониторы"));
	P.Add(StubProduct("Наушники Sony WH-1000XM5", "Sony", "Аудио", "SKU-AU-005", "ART-1005", "электроника,аудио"));
	Return P;
EndFunction

&AtClient
Function StubClient(Name, Inn, City, Segment)
	Return New Structure("name, inn, city, segment", Name, Inn, City, Segment);
EndFunction

&AtClient
Function StubClients()
	C = New Array;
	C.Add(StubClient("ООО ""Ромашка""", "7701000001", "Москва", "опт"));
	C.Add(StubClient("ООО ""Ромашка"" (филиал)", "7701000001", "Москва", "опт"));   // same INN -> dup
	C.Add(StubClient("ИП Иванов И.И.", "500100000010", "Химки", "розница"));
	C.Add(StubClient("ООО ""Рога и Копыта""", "7702000002", "Казань", "опт"));
	C.Add(StubClient("Иванов Иван (ИП)", "500100000010", "Химки", "розница"));      // same INN -> dup
	C.Add(StubClient("ООО ""Василёк""", "7703000003", "Тверь", "розница"));
	Return C;
EndFunction

&AtClient
Function ClientsDedup(Raw)
	// Adapter-side dedup: exact INN match decides "one entity" (NOT in the core).
	Seen = New Map;
	Unique = New Array;
	For Each Cl In Raw Do
		NormKey = TrimAll(Cl.inn);
		If Seen[NormKey] = Undefined Then
			Seen.Insert(NormKey, True);
			Unique.Add(Cl);
		EndIf;
	EndDo;
	Return Unique;
EndFunction

// ============================================================================
// Adapters: map stub domain data -> index_segments payloads with rich metadata.
// ============================================================================

&AtClient
Function StepCatalogPayload()
	Segments = New Array;
	For Each Item In StubStepCatalog() Do
		Seg = New Structure;
		Seg.Insert("text", Item.phrase);
		Seg.Insert("embed_text", Item.phrase + " | " + Item.description + " | параметры: " + Item.paramTypes);
		Seg.Insert("meta", New Structure("type, params", "step", Item.paramTypes));
		Segments.Add(Seg);
	EndDo;
	Return SegmentsPayload("qa_steps", "qa-step-catalog", "QA step catalog", Segments);
EndFunction

&AtClient
Function ScenariosPayload()
	Segments = New Array;
	For Each Sc In StubScenarios() Do
		Verbatim = "Сценарий: " + Sc.name + Chars.LF + Sc.steps;
		Seg = New Structure;
		Seg.Insert("text", Verbatim);
		Seg.Insert("embed_text", Sc.name + " " + StrReplace(Sc.tags, ",", " ") + " " + Sc.steps);
		Seg.Insert("line_start", Sc.lineStart);
		Seg.Insert("line_end", Sc.lineEnd);
		Seg.Insert("meta", New Structure("type, feature, tags, name", "scenario", Sc.feature, Sc.tags, Sc.name));
		Segments.Add(Seg);
	EndDo;
	Return SegmentsPayload("qa_scenarios", "qa-scenarios", "QA scenarios", Segments);
EndFunction

&AtClient
Function ProductsPayload()
	Segments = New Array;
	For Each Pr In StubProducts() Do
		Seg = New Structure;
		// text carries the article/SKU so the keyword channel finds exact ids;
		// embed_text carries name+brand+category for semantic (dense) intent.
		Seg.Insert("text", Pr.name + " (арт. " + Pr.article + ", " + Pr.sku + ")");
		Seg.Insert("embed_text", Pr.name + " | " + Pr.brand + " | " + Pr.category);
		Seg.Insert("meta", New Structure("type, sku, article, category, brand, tags",
			"product", Pr.sku, Pr.article, Pr.category, Pr.brand, Pr.tags));
		Segments.Add(Seg);
	EndDo;
	Return SegmentsPayload("products", "products-catalog", "Products", Segments);
EndFunction

&AtClient
Function ClientsPayload(Unique)
	Segments = New Array;
	For Each Cl In Unique Do
		Seg = New Structure;
		Seg.Insert("text", Cl.name + " (ИНН " + Cl.inn + ")");
		Seg.Insert("embed_text", Cl.name + " " + Cl.city + " " + Cl.segment);
		Seg.Insert("meta", New Structure("type, inn, city, segment", "client", Cl.inn, Cl.city, Cl.segment));
		Segments.Add(Seg);
	EndDo;
	Return SegmentsPayload("clients", "clients-catalog", "Clients", Segments);
EndFunction

&AtClient
Procedure SelfTestAppend(Ctx, Line)

	Ctx.Log.Add(Line);
	// Append ONLY the new line to the result file — O(1) per call. The previous
	// implementation re-concatenated the entire log and truncate-rewrote the whole
	// file on every call (O(N^2) in both string building and I/O). The first line
	// truncates (Append=False) so each run starts clean; the rest append — the
	// same pattern TraceLine uses. Ctx.Log is still kept in memory for any
	// whole-log consumer.
	Try
		IsFirst = (Ctx.Log.Count() = 1);
		Writer = New TextWriter(SelfTestOutFile("ragselftest-result.txt"), TextEncoding.UTF8, Chars.LF, Not IsFirst);
		Writer.WriteLine(Line);
		Writer.Close();
	Except
		// best-effort; nothing to do if the file can't be written
	EndTry;

EndProcedure

#EndRegion


#Region Utilities

&AtClient
Procedure EmptyCallbackHandler(ResultCall, ParametersCall, AdditionalParameters) Export
	// Fire-and-forget callback for BeginCalling* component method calls.
EndProcedure

&AtClient
Function GetArg(Arguments, ParamName)
	
	If TypeOf(Arguments) = Type("Map") Then
		Try
			Return Arguments[ParamName];
		Except
			Return Undefined;
		EndTry;
	ElsIf TypeOf(Arguments) = Type("Structure") Then
		Value = Undefined;
		Arguments.Property(ParamName, Value);
		Return Value;
	Else
		Return Undefined;
	EndIf;
	
	Return Undefined;
	
EndFunction

&AtClient
Function NumberOrDefault(Value, DefaultValue)
	
	If Value = Undefined Then
		Return DefaultValue;
	EndIf;
	
	Return Value;
	
EndFunction

&AtClient
Function ParseJsonArgument(JsonText, DefaultValue)
	
	If Not ValueIsFilled(JsonText) Then
		Return DefaultValue;
	EndIf;
	
	Try
		JSONReader = New JSONReader;
		JSONReader.SetString(JsonText);
		Return ReadJSON(JSONReader, True);
	Except
		Return DefaultValue;
	EndTry;
	
EndFunction

&AtClient
Function SerializeToJson(Value)
	
	JSONWriter = New JSONWriter;
	JSONWriter.SetString();
	WriteJSON(JSONWriter, Value);
	Return JSONWriter.Close();
	
EndFunction

#EndRegion
